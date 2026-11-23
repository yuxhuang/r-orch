use super::*;
use orch_domain::{ReviewCriterion, ReviewResult, TaskProof, WorkspaceMode};

fn task() -> Task {
    serde_json::from_value(serde_json::json!({
        "id":"tsk_Test", "title":"sample", "description":"", "status":"todo", "priority":-3,
        "labels":["b","a","b"], "depends_on":["z","a","z"],
        "created_at":"2026-11-20T10:00:00.000Z", "updated_at":"2026-11-20T10:00:00.000Z",
        "attempts":0, "max_attempts":3,
    }))
    .unwrap()
}

fn detail(text: &str) -> Detail {
    Detail {
        inline: Some(text.into()),
        bytes: text.len() as u64,
        sha256: BlobHash::of(text.as_bytes()).to_string(),
        refs: BTreeMap::new(),
    }
}

#[test]
fn an_absent_proof_and_workspace_mode_stay_absent_and_an_empty_proof_and_a_mode_word_are_kept() {
    let (_temp, data) = crate::sqlite::test_data();
    let store = DiskBlobStore::open(data.root()).unwrap();
    let mut task = task();
    let encoded = encode(&store, &task).unwrap();
    assert!(!encoded.inline.as_ref().unwrap().contains("proof"));
    assert!(!encoded.inline.as_ref().unwrap().contains("workspace_mode"));
    assert_eq!(decode(&store, &encoded).unwrap(), task);
    task.proof = Some(TaskProof::default());
    task.workspace_mode = Some(WorkspaceMode::Worktree);
    task.scope = Some(vec![]);
    task.review_criteria = Some(vec![]);
    task.review_results = Some(vec![]);
    task.attachments = Some(vec![]);
    task.feedback = Some(String::new());
    let encoded = encode(&store, &task).unwrap();
    let json = encoded.inline.as_ref().unwrap();
    assert!(json.contains(r#""proof":{"files_changed":[]}"#));
    assert!(json.contains(r#""workspace_mode":"worktree""#));
    assert_eq!(json, &serde_json::to_string(&task).unwrap());
    assert_eq!(decode(&store, &encoded).unwrap(), task);
}

#[test]
fn explicit_null_and_a_proof_without_files_changed_are_refused() {
    let (_temp, data) = crate::sqlite::test_data();
    let store = DiskBlobStore::open(data.root()).unwrap();
    let task = task();
    for key in [
        "assignee",
        "workspace_mode",
        "workspace",
        "proof",
        "review_criteria",
        "review_results",
        "scope",
        "feedback",
        "goalId",
        "attachments",
    ] {
        let mut json = serde_json::to_value(&task).unwrap();
        json[key] = Value::Null;
        assert!(
            matches!(
                decode(&store, &detail(&json.to_string())),
                Err(CodecError::Invalid { .. })
            ),
            "{key}"
        );
    }
    for proof in [
        serde_json::json!({}),
        serde_json::json!({"files_changed":[], "remote":null}),
        serde_json::json!({"files_changed":[], "extra":false}),
    ] {
        let mut json = serde_json::to_value(&task).unwrap();
        json["proof"] = proof;
        assert!(matches!(
            decode(&store, &detail(&json.to_string())),
            Err(CodecError::Invalid { .. })
        ));
    }
}

#[test]
fn strict_decode_refuses_unknown_duplicate_missing_wrong_and_lossy_values() {
    let (_temp, data) = crate::sqlite::test_data();
    let store = DiskBlobStore::open(data.root()).unwrap();
    let text = serde_json::to_string(&task()).unwrap();
    for (from, to) in [
        (r#""priority":-3"#, r#""priority":-3,"priority":-3"#),
        (r#""priority":-3"#, r#""priority":-3.0"#),
        (r#""priority":-3"#, r#""priority":-3e0"#),
        (r#""priority":-3"#, r#""priority":9223372036854775808"#),
        (r#""attempts":0"#, r#""attempts":4294967296"#),
        (r#""attempts":0"#, r#""attempts":-1"#),
        (r#""max_attempts":3"#, r#""max_attempts":3.0"#),
        (r#""title":"sample""#, r#""title":false"#),
        (r#""status":"todo""#, r#""status":"unknown""#),
        (r#""priority":-3,"labels""#, r#""labels""#),
        (r#""priority":-3"#, r#""priority":-3,"extra":true"#),
    ] {
        assert!(text.contains(from), "test mutation must match: {from}");
        assert!(
            matches!(
                decode(&store, &detail(&text.replace(from, to))),
                Err(CodecError::Invalid { .. })
            ),
            "{to}"
        );
    }
    for priority in [i64::MIN, i64::MAX] {
        let mut task = task();
        task.priority = priority;
        task.attempts = u32::MAX;
        assert_eq!(
            decode(&store, &encode(&store, &task).unwrap()).unwrap(),
            task
        );
    }
    let mut value = serde_json::to_value(task()).unwrap();
    value["review_results"] =
        serde_json::json!([{"criterion":"lint", "passed":true, "output":"x"}]);
    let text = value.to_string();
    for (from, to) in [
        (r#""output":"x""#, r#""output":"x","output":"x"#),
        (r#""passed":true"#, r#""passed":true,"unknown":0"#),
        (r#""criterion":"lint""#, r#""criterion":"invalid""#),
        (r#""passed":true"#, r#""passed":1"#),
    ] {
        assert!(text.contains(from));
        assert!(matches!(
            decode(&store, &detail(&text.replace(from, to))),
            Err(CodecError::Invalid { .. })
        ));
    }
}

#[test]
fn all_nested_text_and_ordered_duplicate_lists_round_trip_with_exact_locators() {
    let (_temp, data) = crate::sqlite::test_data();
    let store = DiskBlobStore::open(data.root()).unwrap();
    let large = "é".repeat(32769);
    let mut task = task();
    task.title = large.clone();
    task.description = large.clone();
    task.workspace = Some(large.clone());
    task.feedback = Some(large.clone());
    task.proof = Some(TaskProof {
        remote: Some(large.clone()),
        branch: Some(large.clone()),
        pr_url: Some(large.clone()),
        files_changed: vec![large.clone(), "b".into(), large.clone()],
        test_results: Some(large.clone()),
        agent_summary: Some(large.clone()),
    });
    task.review_criteria = Some(vec![
        ReviewCriterion::Lint,
        ReviewCriterion::TestPass,
        ReviewCriterion::Lint,
    ]);
    task.review_results = Some(vec![ReviewResult {
        criterion: ReviewCriterion::Lint,
        passed: false,
        output: large.clone(),
    }]);
    task.labels.extend([large.clone(), large.clone()]);
    task.depends_on.extend([large.clone(), large.clone()]);
    task.scope = Some(vec![large.clone(), "x".into(), large.clone()]);
    task.attachments = Some(vec!["../is-only-data".into(), large.clone()]);
    let encoded = encode(&store, &task).unwrap();
    assert_eq!(
        encoded.refs.keys().map(String::as_str).collect::<Vec<_>>(),
        [
            "/attachments/1",
            "/depends_on/3",
            "/depends_on/4",
            "/description",
            "/feedback",
            "/labels/3",
            "/labels/4",
            "/proof/agent_summary",
            "/proof/branch",
            "/proof/files_changed/0",
            "/proof/files_changed/2",
            "/proof/pr_url",
            "/proof/remote",
            "/proof/test_results",
            "/review_results/0/output",
            "/scope/0",
            "/scope/2",
            "/title",
            "/workspace",
        ]
    );
    assert_eq!(decode(&store, &encoded).unwrap(), task);
    let physical = encoded.inline.as_ref().unwrap();
    assert!(!physical.contains(&large));
    assert!(physical.contains("$blob"));
    assert!(
        encoded
            .refs
            .values()
            .all(|rows| rows.len() == 1 && rows[0].size == large.len() as u64)
    );
}

#[test]
fn aggregate_detail_threshold_and_exact_reference_coverage_are_checked() {
    let (_temp, data) = crate::sqlite::test_data();
    let store = DiskBlobStore::open(data.root()).unwrap();
    let mut task = task();
    task.labels = vec!["small".repeat(100); 200];
    let encoded = encode(&store, &task).unwrap();
    assert!(encoded.inline.is_none());
    assert!(encoded.bytes > 65536);
    assert_eq!(
        encoded.refs.keys().map(String::as_str).collect::<Vec<_>>(),
        [""]
    );
    assert_eq!(decode(&store, &encoded).unwrap(), task);
    let mut broken = encoded.clone();
    broken.refs.remove("");
    assert!(matches!(
        decode(&store, &broken),
        Err(CodecError::Invalid { .. })
    ));
    let mut broken = encoded.clone();
    broken
        .refs
        .insert("/unknown".into(), encoded.refs[""].clone());
    assert!(matches!(
        decode(&store, &broken),
        Err(CodecError::Invalid { .. })
    ));
    let mut broken = encoded.clone();
    broken.refs.get_mut("").unwrap()[0].ordinal = 1;
    assert!(matches!(
        decode(&store, &broken),
        Err(CodecError::Invalid { .. })
    ));
    let mut broken = encoded.clone();
    broken.sha256 = "0".repeat(64);
    assert!(matches!(
        decode(&store, &broken),
        Err(CodecError::Invalid { .. })
    ));
}

#[test]
fn key_and_timestamp_validation_precedes_publication() {
    let (_temp, data) = crate::sqlite::test_data();
    let store = DiskBlobStore::open(data.root()).unwrap();
    let mut task = task();
    task.description = "large".repeat(14000);
    for field in ["id", "assignee", "goalId"] {
        let mut value = serde_json::to_value(&task).unwrap();
        value[field] = Value::String("k".repeat(65537));
        let task: Task = serde_json::from_value(value).unwrap();
        assert!(matches!(
            encode(&store, &task),
            Err(CodecError::Unrepresentable(_))
        ));
    }
    for time in [
        "2026-11-20T10:00:00Z",
        "2026-11-20T10:00:00.000000Z",
        "2026-11-20T11:00:00.000+01:00",
        "2026-13-20T10:00:00.000Z",
        "-000001-01-01T00:00:00.000Z",
    ] {
        task.created_at = time.into();
        assert!(matches!(
            encode(&store, &task),
            Err(CodecError::Unrepresentable(_))
        ));
    }
    // An invalid key/time must not even publish the otherwise-valid description.
    assert!(
        store
            .get(&BlobHash::of(task.description.as_bytes()))
            .is_err()
    );
    assert_eq!(
        milliseconds("0000-01-01T00:00:00.000Z").unwrap(),
        -62_167_219_200_000
    );
}

#[test]
fn malformed_tags_wrong_locators_and_unregistered_extra_references_are_refused() {
    let (_temp, data) = crate::sqlite::test_data();
    let store = DiskBlobStore::open(data.root()).unwrap();
    let mut task = task();
    task.description = "a".repeat(65537);
    let encoded = encode(&store, &task).unwrap();
    let mut missing = encoded.clone();
    missing.refs.clear();
    assert!(matches!(
        decode(&store, &missing),
        Err(CodecError::Invalid { .. })
    ));
    let text = encoded.inline.as_ref().unwrap();
    for (from, to) in [
        ("\"chunks\":1", "\"chunks\":1,\"chunks\":1"),
        ("\"chunks\":1", "\"chunks\":1.0"),
        ("\"chunks\":1", "\"chunks\":1,\"extra\":false"),
        ("\"bytes\":65537", "\"bytes\":65536"),
        ("\"$blob\":", "\"$wrong\":"),
    ] {
        assert!(text.contains(from));
        let mut broken = detail(&text.replace(from, to));
        broken.refs = encoded.refs.clone();
        assert!(matches!(
            decode(&store, &broken),
            Err(CodecError::Invalid { .. })
        ));
    }
    let mut broken = encoded.clone();
    broken
        .refs
        .insert("/title".into(), encoded.refs["/description"].clone());
    assert!(matches!(
        decode(&store, &broken),
        Err(CodecError::Invalid { .. })
    ));
}

#[test]
fn inline_root_and_text_boundaries_have_the_prescribed_physical_form() {
    let (_temp, data) = crate::sqlite::test_data();
    let store = DiskBlobStore::open(data.root()).unwrap();
    let mut task = task();
    let fixed = serde_json::to_vec(&task).unwrap().len();
    for length in [65536, 65537] {
        task.description = "a".repeat(length - fixed);
        let encoded = encode(&store, &task).unwrap();
        assert_eq!(encoded.bytes, length as u64);
        assert_eq!(encoded.inline.is_some(), length == 65536);
        assert_eq!(encoded.refs.contains_key(""), length == 65537);
        assert_eq!(decode(&store, &encoded).unwrap(), task);
    }
    for length in [65536, 65537] {
        task.description = "a".repeat(length);
        let encoded = encode(&store, &task).unwrap();
        assert_eq!(encoded.refs.contains_key("/description"), length == 65537);
        // The exactly-inline field can make the aggregate detail indirect.
        assert_eq!(encoded.refs.contains_key(""), length == 65536);
        assert_eq!(decode(&store, &encoded).unwrap(), task);
    }
    // A literal tag-looking user string is plain text and creates no reference.
    task.description = r#"{"$blob":{"sha256":"user text","bytes":99999,"chunks":1}}"#.into();
    let encoded = encode(&store, &task).unwrap();
    assert!(encoded.refs.is_empty());
    assert_eq!(decode(&store, &encoded).unwrap(), task);
}

#[test]
fn transient_object_access_is_distinct_from_a_proven_missing_or_corrupt_object() {
    use std::os::unix::fs::PermissionsExt;
    let (temp, data) = crate::sqlite::test_data();
    let store = DiskBlobStore::open(data.root()).unwrap();
    let mut task = task();
    task.description = "a".repeat(65537);
    let encoded = encode(&store, &task).unwrap();
    let hash = &encoded.refs["/description"][0].sha256;
    let path = temp
        .path()
        .join("data/blobs/sha256")
        .join(&hash[..2])
        .join(&hash[2..4])
        .join(hash);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
    let error = decode(&store, &encoded).unwrap_err();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(
        matches!(
            error,
            CodecError::Access {
                kind: std::io::ErrorKind::PermissionDenied,
                ..
            }
        ),
        "{error}"
    );
    assert!(
        !error
            .to_string()
            .contains(&temp.path().display().to_string())
    );
    std::fs::write(&path, b"corrupt").unwrap();
    assert!(matches!(
        decode(&store, &encoded),
        Err(CodecError::Invalid {
            check: "referenced bytes",
            ..
        })
    ));
    std::fs::remove_file(path).unwrap();
    assert!(matches!(
        decode(&store, &encoded),
        Err(CodecError::Invalid {
            check: "referenced bytes",
            ..
        })
    ));
}

#[test]
fn an_aggregate_detail_over_one_object_reconstructs_before_decoding_the_task() {
    let (_temp, data) = crate::sqlite::test_data();
    let store = DiskBlobStore::open(data.root()).unwrap();
    let mut task = task();
    task.labels = vec!["a".repeat(65536); 1024];
    let encoded = encode(&store, &task).unwrap();
    assert!(encoded.inline.is_none());
    assert!(encoded.bytes > MAX_BLOB_BYTES);
    assert_eq!(encoded.refs.len(), 1);
    assert_eq!(encoded.refs[""].len(), 2);
    assert_eq!(encoded.refs[""][0].size, MAX_BLOB_BYTES);
    assert_eq!(encoded.refs[""][1].size, encoded.bytes - MAX_BLOB_BYTES);
    assert_eq!(decode(&store, &encoded).unwrap(), task);
    let mut swapped = encoded;
    swapped.refs.get_mut("").unwrap().swap(0, 1);
    assert!(matches!(
        decode(&store, &swapped),
        Err(CodecError::Invalid { .. })
    ));
}

#[test]
fn stored_noncanonical_time_and_oversized_inline_text_are_typed_corruption() {
    let (_temp, data) = crate::sqlite::test_data();
    let store = DiskBlobStore::open(data.root()).unwrap();
    let mut value = serde_json::to_value(task()).unwrap();
    value["created_at"] = "2026-11-20T10:00:00Z".into();
    assert!(matches!(
        decode(&store, &detail(&value.to_string())),
        Err(CodecError::Invalid {
            check: "keys or timestamps",
            ..
        })
    ));
    value = serde_json::to_value(task()).unwrap();
    value["title"] = "a".repeat(65537).into();
    let bytes = serde_json::to_vec(&value).unwrap();
    let (reference, rows) = text::publish(&store, &bytes).unwrap();
    let stored = Detail {
        inline: None,
        bytes: reference.bytes,
        sha256: reference.sha256,
        refs: BTreeMap::from([(String::new(), rows)]),
    };
    assert!(matches!(decode(&store, &stored),
        Err(CodecError::Invalid { check:"inline text size", locator }) if locator=="/title"));
}

#[test]
fn all_reference_metadata_is_checked_before_any_slot_object_access() {
    use std::os::unix::fs::PermissionsExt;
    let (temp, data) = crate::sqlite::test_data();
    let store = DiskBlobStore::open(data.root()).unwrap();
    let mut task = task();
    task.description = "a".repeat(65537);
    task.feedback = Some("b".repeat(65537));
    let mut encoded = encode(&store, &task).unwrap();
    let hash = &encoded.refs["/description"][0].sha256;
    let path = temp
        .path()
        .join("data/blobs/sha256")
        .join(&hash[..2])
        .join(&hash[2..4])
        .join(hash);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
    encoded
        .refs
        .insert("/title".into(), encoded.refs["/description"].clone());
    let coverage = decode(&store, &encoded);
    encoded.refs.remove("/title");
    encoded.refs.get_mut("/feedback").unwrap()[0].ordinal = 1;
    let metadata = decode(&store, &encoded);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(matches!(
        coverage,
        Err(CodecError::Invalid {
            check: "reference locators",
            ..
        })
    ));
    assert!(
        matches!(metadata, Err(CodecError::Invalid { check:"reference metadata", locator }) if locator=="/feedback")
    );
}

#[test]
fn typed_damage_is_checked_before_any_slot_object_access() {
    use std::os::unix::fs::PermissionsExt;
    let (temp, data) = crate::sqlite::test_data();
    let store = DiskBlobStore::open(data.root()).unwrap();
    let mut task = task();
    task.description = "a".repeat(65537);
    let encoded = encode(&store, &task).unwrap();
    let hash = &encoded.refs["/description"][0].sha256;
    let path = temp
        .path()
        .join("data/blobs/sha256")
        .join(&hash[..2])
        .join(&hash[2..4])
        .join(hash);
    let original: Value = serde_json::from_str(encoded.inline.as_ref().unwrap()).unwrap();
    for (field, damage, check) in [
        ("priority", Value::from("high"), "typed detail"),
        ("status", Value::from("bogus"), "typed detail"),
        ("attempts", Value::from(-1), "typed detail"),
        (
            "created_at",
            Value::from("2026-11-20T10:00:00Z"),
            "keys or timestamps",
        ),
    ] {
        let mut value = original.clone();
        value[field] = damage;
        let mut broken = detail(&value.to_string());
        broken.refs = encoded.refs.clone();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
        let control = decode(&store, &encoded);
        let damaged = decode(&store, &broken);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(matches!(
            control,
            Err(CodecError::Access {
                kind: std::io::ErrorKind::PermissionDenied,
                ..
            })
        ));
        assert!(
            matches!(damaged, Err(CodecError::Invalid { check: actual, .. }) if actual == check),
            "{field}: {damaged:?}"
        );
    }
    assert_eq!(decode(&store, &encoded).unwrap(), task);
}
