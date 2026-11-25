use super::*;
use serde_json::json;

fn agent() -> Agent {
    serde_json::from_value(json!({"id":"a", "name":"same", "adapter":"codex", "config":{},
        "status":"idle", "stats":{"tasks_completed":0,"tasks_failed":0,"total_runs":0,"total_runtime_ms":0}})).unwrap()
}
fn team() -> Team {
    serde_json::from_value(
        json!({"id":"t","name":"same","status":"active","members":[],"task_pool":[],
        "lead_agent_id":"a","created_at":"opaque","updated_at":"2026-11-20T10:00:00.1239+02:00",
        "config":{"auto_claim":false}}),
    )
    .unwrap()
}
fn run() -> Run {
    serde_json::from_value(
        json!({"id":"r","task_id":"t","agent_id":"a","attempt":10,"status":"running",
        "started_at":"opaque","workspace_path":"/synthetic","prompt":""}),
    )
    .unwrap()
}
fn detail(value: &str) -> Detail {
    Detail {
        inline: Some(value.into()),
        bytes: value.len() as u64,
        sha256: BlobHash::of(value.as_bytes()).to_string(),
        refs: BTreeMap::new(),
    }
}

#[test]
fn complete_values_preserve_absence_empty_false_order_and_unsigned_limits() {
    let (_temp, data) = crate::sqlite::test_data();
    let store = DiskBlobStore::open(data.root()).unwrap();
    let mut a = agent();
    for autonomous in [None, Some(false), Some(true)] {
        a.autonomous = autonomous;
        a.role = Some(String::new());
        a.config.env = Some(
            [("Z".into(), "".into()), ("A".into(), "v".into())]
                .into_iter()
                .collect(),
        );
        a.config.skills = Some(vec![]);
        a.config.timeout_ms = Some(u64::MAX);
        a.stats.total_runtime_ms = u64::MAX;
        a.capabilities = vec!["b".into(), "a".into(), "b".into()];
        let d = encode_agent(&store, &a).unwrap();
        assert!(
            d.inline
                .as_ref()
                .unwrap()
                .contains(r#""env":[["Z",""],["A","v"]]"#)
        );
        assert_eq!(decode_agent(&store, &d).unwrap(), a);
    }
    let mut t = team();
    t.members = serde_json::from_value(json!([{"agent_id":"a","role":"lead","joined_at":"x"},
        {"agent_id":"a","role":"member","joined_at":""}]))
    .unwrap();
    t.task_pool = vec!["b".into(), "a".into(), "b".into()];
    assert_eq!(
        decode_team(&store, &encode_team(&store, &t).unwrap()).unwrap(),
        t
    );
    let mut r = run();
    r.attempt = u32::MAX;
    r.pid = Some(0);
    r.tokens=serde_json::from_value(json!({"input":1,"output":2,"reasoning":3,"total":99,"cache_read":0,"cache_write":u64::MAX})).ok();
    assert_eq!(
        decode_run(&store, &encode_run(&store, &r).unwrap()).unwrap(),
        r
    );
}

#[test]
fn empty_capabilities_are_omitted_and_empty_prompt_is_one_zero_byte_object() {
    let (_temp, data) = crate::sqlite::test_data();
    let store = DiskBlobStore::open(data.root()).unwrap();
    let a = agent();
    let d = encode_agent(&store, &a).unwrap();
    assert!(!d.inline.as_ref().unwrap().contains("capabilities"));
    assert_eq!(decode_agent(&store, &d).unwrap(), a);
    let r = run();
    let d = encode_run(&store, &r).unwrap();
    assert_eq!(d.refs["/prompt"].len(), 1);
    assert_eq!(d.refs["/prompt"][0].size, 0);
    assert_eq!(d.refs["/prompt"][0].sha256, BlobHash::of(b"").to_string());
    assert_eq!(decode_run(&store, &d).unwrap(), r);
}

#[test]
fn strict_agent_decoder_rejects_unknown_duplicate_null_missing_and_nonintegers() {
    let (_temp, data) = crate::sqlite::test_data();
    let store = DiskBlobStore::open(data.root()).unwrap();
    let text = serde_json::to_string(&agent()).unwrap();
    for (from, to) in [
        (r#""id":"a""#, r#""id":"a","id":"a""#),
        (r#""id":"a""#, r#""id":"a","extra":false"#),
        (r#""config":{}"#, r#""config":{"model":null}"#),
        (r#""config":{}"#, r#""config":{"extra":1}"#),
        (r#""config":{}"#, r#""config":{"max_turns":4294967296}"#),
        (r#""total_runs":0"#, r#""total_runs":0.0"#),
        (r#""total_runs":0"#, r#""total_runs":0e0"#),
        (r#""total_runs":0"#, r#""total_runs":-1"#),
        (r#""total_runs":0"#, r#""total_runs":18446744073709551616"#),
        (r#""status":"idle""#, r#""status":"alien""#),
    ] {
        let bad = text.replace(from, to);
        assert_ne!(bad, text);
        assert!(decode_agent(&store, &detail(&bad)).is_err(), "{to}");
    }
    let mut v = serde_json::to_value(agent()).unwrap();
    v.as_object_mut().unwrap().remove("stats");
    assert!(decode_agent(&store, &detail(&v.to_string())).is_err());
}

#[test]
fn every_nested_string_uses_original_byte_limits_and_oversized_ids_roundtrip() {
    let (_temp, data) = crate::sqlite::test_data();
    let store = DiskBlobStore::open(data.root()).unwrap();
    let mut a = agent();
    a.id = "i".repeat(65537);
    a.role = Some("é".repeat(32768));
    a.name = "n".repeat(65537);
    a.adapter = "c".repeat(65537);
    a.current_task = Some("t".repeat(65537));
    a.config.command = Some("c".repeat(65537));
    a.config.system_prompt = Some("p".repeat(65537));
    a.config.model = Some("m".repeat(65537));
    a.config.role_summary = Some("s".repeat(65537));
    a.config.env = Some(
        [("k".repeat(65537), "v".repeat(65537))]
            .into_iter()
            .collect(),
    );
    a.config.skills = Some(vec!["s".repeat(65537)]);
    a.capabilities = vec!["c".repeat(65537)];
    a.last_error = serde_json::from_value(
        json!({"message":"m".repeat(65537),"kind":"k".repeat(65537),"timestamp":"x".repeat(65537)}),
    )
    .ok();
    a.remote = serde_json::from_value(json!({"alias":"r".repeat(65537),"online":false})).ok();
    let d = encode_agent(&store, &a).unwrap();
    for path in [
        "/id",
        "/name",
        "/adapter",
        "/current_task",
        "/config/command",
        "/config/system_prompt",
        "/config/model",
        "/config/role_summary",
        "/config/env/0/0",
        "/config/env/0/1",
        "/config/skills/0",
        "/capabilities/0",
        "/last_error/message",
        "/last_error/kind",
        "/last_error/timestamp",
        "/remote/alias",
    ] {
        assert_eq!(d.refs[path][0].size, 65537, "{path}");
        let hash = d.refs[path][0].sha256.parse().unwrap();
        assert_eq!(store.get(&hash).unwrap().len(), 65537);
    }
    assert!(!d.refs.contains_key("/role"));
    assert_eq!(decode_agent(&store, &d).unwrap(), a);
    a.role.as_mut().unwrap().push('a');
    let d = encode_agent(&store, &a).unwrap();
    assert_eq!(d.refs["/role"][0].size, 65537);
    assert_eq!(decode_agent(&store, &d).unwrap(), a);
}

#[test]
fn aggregate_detail_is_indirect_even_when_every_string_is_small() {
    let (_temp, data) = crate::sqlite::test_data();
    let store = DiskBlobStore::open(data.root()).unwrap();
    let mut t = team();
    t.task_pool = vec!["a".repeat(40000), "b".repeat(40000)];
    let d = encode_team(&store, &t).unwrap();
    assert!(d.inline.is_none());
    assert_eq!(d.refs.len(), 1);
    assert_eq!(d.refs[""][0].size, d.bytes);
    assert_eq!(decode_team(&store, &d).unwrap(), t);
}

#[test]
fn team_and_run_nested_slots_preserve_exact_values() {
    let (_temp, data) = crate::sqlite::test_data();
    let store = DiskBlobStore::open(data.root()).unwrap();
    let big = "é".repeat(32769);
    let mut t = team();
    t.description = Some(big.clone());
    t.created_at = big.clone();
    t.updated_at = big.clone();
    t.members =
        serde_json::from_value(json!([{"agent_id":big,"role":"member","joined_at":big}])).unwrap();
    let d = encode_team(&store, &t).unwrap();
    for key in [
        "/description",
        "/created_at",
        "/updated_at",
        "/members/0/agent_id",
        "/members/0/joined_at",
    ] {
        assert_eq!(d.refs[key][0].size, big.len() as u64);
    }
    assert_eq!(decode_team(&store, &d).unwrap(), t);
    let mut r = run();
    r.task_id = big.clone();
    r.agent_id = big.clone();
    r.started_at = big.clone();
    r.finished_at = Some(big.clone());
    r.workspace_path = big.clone();
    r.error = Some(big.clone());
    r.prompt = big.clone();
    let d = encode_run(&store, &r).unwrap();
    for key in [
        "/task_id",
        "/agent_id",
        "/started_at",
        "/finished_at",
        "/workspace_path",
        "/error",
        "/prompt",
    ] {
        assert_eq!(d.refs[key][0].size, big.len() as u64);
    }
    assert_eq!(decode_run(&store, &d).unwrap(), r);
}

#[test]
fn malformed_detail_and_references_are_refused_with_bounded_diagnostics() {
    let (_temp, data) = crate::sqlite::test_data();
    let store = DiskBlobStore::open(data.root()).unwrap();
    let mut a = agent();
    a.role = Some("x".repeat(65537));
    let good = encode_agent(&store, &a).unwrap();
    for case in 0..8 {
        let mut d = good.clone();
        match case {
            0 => d.bytes += 1,
            1 => d.sha256 = "0".repeat(64),
            2 => d.refs.get_mut("/role").unwrap()[0].ordinal = 1,
            3 => d.refs.get_mut("/role").unwrap()[0].size -= 1,
            4 => d.refs.get_mut("/role").unwrap()[0].sha256 = "SECRET_INVALID_HASH".into(),
            5 => {
                d.refs.insert("/unknown".into(), d.refs["/role"].clone());
            }
            6 => {
                d.refs.remove("/role");
            }
            _ => {
                let row = d.refs["/role"][0].clone();
                d.refs.get_mut("/role").unwrap().push(row);
            }
        }
        let error = decode_agent(&store, &d).unwrap_err();
        assert!(matches!(error, FormatError::Invalid { .. }), "case {case}");
        assert!(!error.to_string().contains("SECRET"));
    }
    let mut v: Value = serde_json::from_str(good.inline.as_ref().unwrap()).unwrap();
    v["role"] = json!({"$blob":{"sha256":"f".repeat(64),"bytes":65537,"chunks":1,"extra":false}});
    assert!(decode_agent(&store, &detail(&v.to_string())).is_err());
}

#[test]
fn inline_enum_and_counter_damage_is_proven_before_missing_slot_access() {
    let (_temp, data) = crate::sqlite::test_data();
    let store = DiskBlobStore::open(data.root()).unwrap();
    let mut a = agent();
    a.role = Some("x".repeat(65537));
    let d = encode_agent(&store, &a).unwrap();
    let mut v: Value = serde_json::from_str(d.inline.as_ref().unwrap()).unwrap();
    let missing = "0".repeat(64);
    v["role"]["$blob"]["sha256"] = missing.clone().into();
    let mut d = detail(&v.to_string());
    d.refs.insert(
        "/role".into(),
        vec![Chunk {
            ordinal: 0,
            sha256: missing,
            size: 65537,
        }],
    );
    assert!(matches!(
        decode_agent(&store, &d),
        Err(FormatError::Invalid {
            check: "referenced bytes",
            ..
        })
    ));
    for (key, bad) in [
        ("status", json!("alien")),
        (
            "stats",
            json!({"tasks_completed":0,"tasks_failed":0,"total_runs":-1,"total_runtime_ms":0}),
        ),
    ] {
        let mut broken = v.clone();
        broken[key] = bad;
        let mut broken = detail(&broken.to_string());
        broken.refs = d.refs.clone();
        assert!(
            matches!(
                decode_agent(&store, &broken),
                Err(FormatError::Invalid {
                    check: "typed detail",
                    ..
                })
            ),
            "{key}"
        );
    }
}

#[test]
fn environment_pairs_refuse_duplicates_wrong_shapes_null_and_unknown_nested_fields() {
    let (_temp, data) = crate::sqlite::test_data();
    let store = DiskBlobStore::open(data.root()).unwrap();
    for env in [
        json!({"A":"v"}),
        json!([["A", "1"], ["A", "2"]]),
        json!([["A"]]),
        json!([["A", "1", "x"]]),
        json!([[false, "v"]]),
        json!([["A", null]]),
    ] {
        let mut v = serde_json::to_value(agent()).unwrap();
        v["config"]["env"] = env;
        assert!(decode_agent(&store, &detail(&v.to_string())).is_err());
    }
    for (key, bad) in [
        ("remote", json!({"alias":"a","online":false,"extra":1})),
        (
            "last_error",
            json!({"message":"x","kind":"y","timestamp":"z","extra":true}),
        ),
        (
            "stats",
            json!({"tasks_completed":0,"tasks_failed":0,"total_runs":0,"total_runtime_ms":0,"extra":1}),
        ),
    ] {
        let mut v = serde_json::to_value(agent()).unwrap();
        v[key] = bad;
        assert!(decode_agent(&store, &detail(&v.to_string())).is_err());
    }
}

#[test]
fn team_and_run_reject_unknown_missing_null_and_wrong_nested_types() {
    let (_temp, data) = crate::sqlite::test_data();
    let store = DiskBlobStore::open(data.root()).unwrap();
    for (key, bad) in [
        (
            "members",
            json!([{"agent_id":"a","role":"member","joined_at":"x","extra":true}]),
        ),
        ("config", json!({"auto_claim":false,"extra":false})),
        ("task_pool", json!([null])),
        ("status", json!("alien")),
        ("description", Value::Null),
    ] {
        let mut v = serde_json::to_value(team()).unwrap();
        v[key] = bad;
        assert!(decode_team(&store, &detail(&v.to_string())).is_err());
    }
    let r = run();
    let good = encode_run(&store, &r).unwrap();
    for (key, bad) in [
        ("attempt", json!(4294967296u64)),
        ("pid", json!(-1)),
        ("tokens", json!({"input":0})),
        ("status", json!("alien")),
        ("finished_at", Value::Null),
        ("prompt", json!("inline")),
    ] {
        let mut v: Value = serde_json::from_str(good.inline.as_ref().unwrap()).unwrap();
        v[key] = bad;
        let mut d = detail(&v.to_string());
        d.refs = good.refs.clone();
        assert!(decode_run(&store, &d).is_err(), "{key}");
    }
}

#[test]
fn prompt_sizes_short_inline_limit_and_one_more_always_name_verified_objects() {
    let (_temp, data) = crate::sqlite::test_data();
    let store = DiskBlobStore::open(data.root()).unwrap();
    for size in [1, 65536, 65537] {
        let mut r = run();
        r.prompt = "p".repeat(size);
        let d = encode_run(&store, &r).unwrap();
        assert_eq!(d.refs["/prompt"].len(), 1);
        assert_eq!(d.refs["/prompt"][0].size, size as u64);
        assert_eq!(
            store.get(&BlobHash::of(r.prompt.as_bytes())).unwrap(),
            r.prompt.as_bytes()
        );
        assert_eq!(decode_run(&store, &d).unwrap(), r);
    }
}

#[test]
fn split_utf8_prompt_and_exact_object_limit_reconstruct_from_the_manifest() {
    let (_temp, data) = crate::sqlite::test_data();
    let store = DiskBlobStore::open(data.root()).unwrap();
    let mut r = run();
    r.prompt = "a".repeat(MAX_BLOB_BYTES as usize);
    let d = encode_run(&store, &r).unwrap();
    assert_eq!(d.refs["/prompt"].len(), 1);
    assert_eq!(d.refs["/prompt"][0].size, MAX_BLOB_BYTES);
    assert_eq!(decode_run(&store, &d).unwrap(), r);
    r.prompt.pop();
    r.prompt.push('é');
    let d = encode_run(&store, &r).unwrap();
    assert_eq!(
        d.refs["/prompt"]
            .iter()
            .map(|row| row.size)
            .collect::<Vec<_>>(),
        vec![MAX_BLOB_BYTES, 1]
    );
    assert!(
        std::str::from_utf8(
            &store
                .get(&d.refs["/prompt"][0].sha256.parse().unwrap())
                .unwrap()
        )
        .is_err()
    );
    assert_eq!(decode_run(&store, &d).unwrap(), r);
    let mut swapped = d.clone();
    swapped.refs.get_mut("/prompt").unwrap().swap(0, 1);
    assert!(decode_run(&store, &swapped).is_err());
    let mut truncated = d;
    truncated.refs.get_mut("/prompt").unwrap().pop();
    assert!(decode_run(&store, &truncated).is_err());
}

#[test]
fn repeated_slot_hashes_keep_distinct_locators_and_env_names_are_checked_after_reconstruction() {
    let (_temp, data) = crate::sqlite::test_data();
    let store = DiskBlobStore::open(data.root()).unwrap();
    let mut a = agent();
    a.config.env = Some(
        [
            ("a".repeat(65537), "same".repeat(20000)),
            ("b".repeat(65537), "same".repeat(20000)),
        ]
        .into_iter()
        .collect(),
    );
    let d = encode_agent(&store, &a).unwrap();
    assert_eq!(d.refs["/config/env/0/1"], d.refs["/config/env/1/1"]);
    assert_eq!(decode_agent(&store, &d).unwrap(), a);
    let mut v: Value = serde_json::from_str(d.inline.as_ref().unwrap()).unwrap();
    v["config"]["env"][1][0] = v["config"]["env"][0][0].clone();
    let mut broken = detail(&v.to_string());
    broken.refs = d.refs.clone();
    broken
        .refs
        .insert("/config/env/1/0".into(), d.refs["/config/env/0/0"].clone());
    assert!(matches!(
        decode_agent(&store, &broken),
        Err(FormatError::Invalid {
            check: "environment value or duplicate",
            ..
        })
    ));
}

#[test]
fn permission_errors_stay_ordinary_until_metadata_or_inline_types_prove_corruption() {
    use std::os::unix::fs::PermissionsExt;
    let (temp, data) = crate::sqlite::test_data();
    let store = DiskBlobStore::open(data.root()).unwrap();
    let mut r = run();
    r.prompt = "short".into();
    r.error = Some("e".repeat(65537));
    let good = encode_run(&store, &r).unwrap();
    let hash = &good.refs["/prompt"][0].sha256;
    let path = temp
        .path()
        .join("data/blobs/sha256")
        .join(&hash[..2])
        .join(&hash[2..4])
        .join(hash);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
    let access = decode_run(&store, &good);
    let mut broken = good.clone();
    broken.refs.get_mut("/error").unwrap()[0].ordinal = 1;
    let metadata = decode_run(&store, &broken);
    let mut v: Value = serde_json::from_str(good.inline.as_ref().unwrap()).unwrap();
    v["attempt"] = json!(-1);
    let mut broken = detail(&v.to_string());
    broken.refs = good.refs.clone();
    let types = decode_run(&store, &broken);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(
        matches!(
            access,
            Err(FormatError::Access {
                kind: std::io::ErrorKind::PermissionDenied,
                ..
            })
        ),
        "{access:?}"
    );
    assert!(matches!(
        metadata,
        Err(FormatError::Invalid {
            check: "reference metadata",
            ..
        })
    ));
    assert!(matches!(
        types,
        Err(FormatError::Invalid {
            check: "typed detail",
            ..
        })
    ));
    assert_eq!(decode_run(&store, &good).unwrap(), r);
    std::fs::write(&path, b"damaged").unwrap();
    let error = decode_run(&store, &good).unwrap_err();
    assert!(matches!(error, FormatError::Invalid { .. }));
    assert!(
        !error
            .to_string()
            .contains(&temp.path().display().to_string())
    );
}

#[test]
fn all_statuses_empty_and_present_zero_tokens_remain_distinct() {
    let (_temp, data) = crate::sqlite::test_data();
    let store = DiskBlobStore::open(data.root()).unwrap();
    let mut a = agent();
    for status in orch_domain::AgentStatus::ALL {
        a.status = status;
        assert_eq!(
            decode_agent(&store, &encode_agent(&store, &a).unwrap()).unwrap(),
            a
        );
    }
    let mut t = team();
    for status in orch_domain::TeamStatus::ALL {
        t.status = status;
        assert_eq!(
            decode_team(&store, &encode_team(&store, &t).unwrap()).unwrap(),
            t
        );
    }
    let mut r = run();
    for status in orch_domain::RunStatus::ALL {
        for tokens in [None, Some(orch_domain::TokenUsage::default())] {
            r.status = status;
            r.tokens = tokens;
            assert_eq!(
                decode_run(&store, &encode_run(&store, &r).unwrap()).unwrap(),
                r
            );
        }
    }
}

fn indirect(store: &DiskBlobStore, text: &str) -> Detail {
    let (_, chunks) = text::publish(store, text.as_bytes()).unwrap();
    let mut refs = BTreeMap::new();
    refs.insert(String::new(), chunks);
    Detail {
        inline: None,
        bytes: text.len() as u64,
        sha256: BlobHash::of(text.as_bytes()).to_string(),
        refs,
    }
}

#[test]
fn short_prompt_manifest_damage_is_refused() {
    let (_temp, data) = crate::sqlite::test_data();
    let store = DiskBlobStore::open(data.root()).unwrap();
    let mut r = run();
    r.prompt = "short".into();
    let good = encode_run(&store, &r).unwrap();
    let other = store.put(b"other").unwrap().to_string();
    let mut out = Vec::new();
    for case in 0..5 {
        let mut d = good.clone();
        let mut v: Value = serde_json::from_str(d.inline.as_ref().unwrap()).unwrap();
        match case {
            0 => d.refs.get_mut("/prompt").unwrap()[0].sha256 = other.clone(),
            1 => d.refs.get_mut("/prompt").unwrap()[0].size = 4,
            2 => {
                v["prompt"]["$blob"]["chunks"] = json!(0);
                d = Detail {
                    refs: d.refs.clone(),
                    ..detail(&v.to_string())
                };
            }
            3 => {
                v["prompt"]["$blob"]["bytes"] = json!(4);
                d = Detail {
                    refs: {
                        let mut x = d.refs.clone();
                        x.get_mut("/prompt").unwrap()[0].size = 4;
                        x
                    },
                    ..detail(&v.to_string())
                };
            }
            _ => {
                v["prompt"]["$blob"]["chunks"] = json!(2);
                d = Detail {
                    refs: d.refs.clone(),
                    ..detail(&v.to_string())
                };
            }
        }
        out.push(format!(
            "{case}:{:?}",
            decode_run(&store, &d).err().map(|e| e.to_string())
        ));
        assert!(decode_run(&store, &d).is_err(), "case {case}");
    }
    eprintln!("P01 {out:?}");
}

#[test]
fn oversize_inline_text_inside_indirect_root_is_refused() {
    let (_temp, data) = crate::sqlite::test_data();
    let store = DiskBlobStore::open(data.root()).unwrap();
    let mut v = serde_json::to_value(team()).unwrap();
    v["task_pool"] = json!(["a".repeat(65537)]);
    let r = decode_team(&store, &indirect(&store, &v.to_string()));
    eprintln!("P02 {:?}", r.as_ref().err().map(|e| e.to_string()));
    assert!(r.is_err());
}

#[test]
fn aggregate_exact_boundary() {
    let (_temp, data) = crate::sqlite::test_data();
    let store = DiskBlobStore::open(data.root()).unwrap();
    let mut t = team();
    t.task_pool = vec![String::new(), String::new()];
    let base = serde_json::to_vec(&t).unwrap().len();
    let need = 65536 - base;
    t.task_pool = vec!["a".repeat(need / 2), "b".repeat(need - need / 2)];
    let d = encode_team(&store, &t).unwrap();
    assert_eq!(d.bytes, 65536);
    assert!(d.inline.is_some() && d.refs.is_empty());
    assert_eq!(decode_team(&store, &d).unwrap(), t);
    t.task_pool[1].push('c');
    let d = encode_team(&store, &t).unwrap();
    assert_eq!(d.bytes, 65537);
    assert!(d.inline.is_none());
    assert_eq!(d.refs[""].len(), 1);
    assert_eq!(decode_team(&store, &d).unwrap(), t);
}

#[test]
fn inline_prompt_without_refs_is_refused() {
    let (_temp, data) = crate::sqlite::test_data();
    let store = DiskBlobStore::open(data.root()).unwrap();
    let v = serde_json::to_value(run()).unwrap();
    assert!(matches!(
        decode_run(&store, &detail(&v.to_string())),
        Err(FormatError::Invalid {
            check: "inline text size or prompt",
            ..
        })
    ));
}

#[test]
fn oversized_inline_root_is_refused_even_when_all_slots_fit() {
    let (_temp, data) = crate::sqlite::test_data();
    let store = DiskBlobStore::open(data.root()).unwrap();
    let mut t = team();
    t.task_pool = vec!["a".repeat(33000), "b".repeat(33000)];
    assert!(matches!(
        decode_team(&store, &detail(&serde_json::to_string(&t).unwrap())),
        Err(FormatError::Invalid {
            check: "inline detail size",
            ..
        })
    ));
}

#[test]
fn tokens_unknown_fields_are_refused() {
    let (_temp, data) = crate::sqlite::test_data();
    let store = DiskBlobStore::open(data.root()).unwrap();
    let mut r = run();
    r.tokens = Some(Default::default());
    let good = encode_run(&store, &r).unwrap();
    let mut v: Value = serde_json::from_str(good.inline.as_ref().unwrap()).unwrap();
    v["tokens"]["extra"] = json!(0);
    let mut d = detail(&v.to_string());
    d.refs = good.refs;
    assert!(decode_run(&store, &d).is_err());
}

#[test]
fn shared_reader_keeps_null_fraction_exponent_and_nested_duplicate_refusals() {
    let (_temp, data) = crate::sqlite::test_data();
    let store = DiskBlobStore::open(data.root()).unwrap();
    let mut r = run();
    r.tokens = Some(Default::default());
    let good = encode_run(&store, &r).unwrap();
    let text = good.inline.as_ref().unwrap();
    for (from, to) in [
        (r#""attempt":10"#, r#""attempt":10.0"#),
        (r#""attempt":10"#, r#""attempt":1e1"#),
        (r#""attempt":10"#, r#""attempt":null"#),
        (r#""input":0"#, r#""input":0,"input":0"#),
        (r#""input":0"#, r#""input":0.0"#),
        (r#""input":0"#, r#""input":0e0"#),
        (r#""input":0"#, r#""input":null"#),
    ] {
        let bad = text.replace(from, to);
        assert_ne!(&bad, text);
        let mut d = detail(&bad);
        d.refs = good.refs.clone();
        assert!(decode_run(&store, &d).is_err(), "{to}");
    }
}

// #252: presence is meaningful even for optional scalar projections. These
// permanent cases must precede any SQL mapping claims.
fn rejects_optional_null(family: Family, pointer: &str) {
    let (_temp, data) = crate::sqlite::test_data();
    let store = DiskBlobStore::open(data.root()).unwrap();
    let encoded = match family {
        Family::Agent => encode_agent(&store, &agent()),
        Family::Team => encode_team(&store, &team()),
        Family::Run => encode_run(&store, &run()),
    }
    .unwrap();
    let mut value: serde_json::Value =
        serde_json::from_str(encoded.inline.as_ref().unwrap()).unwrap();
    let (parent, key) = pointer.rsplit_once('/').unwrap();
    value
        .pointer_mut(parent)
        .unwrap()
        .as_object_mut()
        .unwrap()
        .insert(key.into(), serde_json::Value::Null);
    let mut invalid = detail(&value.to_string());
    invalid.refs = encoded.refs;
    let result = match family {
        Family::Agent => decode_agent(&store, &invalid).map(|_| ()),
        Family::Team => decode_team(&store, &invalid).map(|_| ()),
        Family::Run => decode_run(&store, &invalid).map(|_| ()),
    };
    assert!(result.is_err(), "accepted explicit null at {pointer}");
}

#[test]
fn optional_null_agent_autonomous_is_refused() {
    rejects_optional_null(Family::Agent, "/autonomous");
}
#[test]
fn optional_null_agent_max_turns_is_refused() {
    rejects_optional_null(Family::Agent, "/config/max_turns");
}
#[test]
fn optional_null_agent_timeout_ms_is_refused() {
    rejects_optional_null(Family::Agent, "/config/timeout_ms");
}
#[test]
fn optional_null_agent_stall_timeout_ms_is_refused() {
    rejects_optional_null(Family::Agent, "/config/stall_timeout_ms");
}
#[test]
fn optional_null_agent_tokens_used_is_refused() {
    rejects_optional_null(Family::Agent, "/stats/tokens_used");
}
#[test]
fn optional_null_team_max_concurrent_tasks_is_refused() {
    rejects_optional_null(Family::Team, "/config/max_concurrent_tasks");
}
#[test]
fn optional_null_team_message_ttl_ms_is_refused() {
    rejects_optional_null(Family::Team, "/config/message_ttl_ms");
}
#[test]
fn optional_null_run_pid_is_refused() {
    rejects_optional_null(Family::Run, "/pid");
}

#[test]
fn value_equivalent_detail_whitespace_key_order_and_empty_capabilities_are_readable() {
    let (_temp, data) = crate::sqlite::test_data();
    let store = DiskBlobStore::open(data.root()).unwrap();
    let encoded = encode_agent(&store, &agent()).unwrap();
    let value: serde_json::Value = serde_json::from_str(encoded.inline.as_ref().unwrap()).unwrap();
    // Deliberately reverse insertion order rather than comparing encoder bytes.
    let mut reordered = serde_json::Map::new();
    for (key, value) in value.as_object().unwrap().iter().rev() {
        reordered.insert(key.clone(), value.clone());
    }
    reordered.insert("capabilities".into(), json!([]));
    let equivalent = serde_json::to_string_pretty(&reordered).unwrap();
    assert_ne!(equivalent, encoded.inline.unwrap());
    assert_eq!(decode_agent(&store, &detail(&equivalent)).unwrap(), agent());
}

#[test]
fn empty_prompt_requires_exactly_one_chunk_at_ordinal_zero() {
    let (_temp, data) = crate::sqlite::test_data();
    let store = DiskBlobStore::open(data.root()).unwrap();
    let r = run();
    let valid = encode_run(&store, &r).unwrap();
    assert_eq!(decode_run(&store, &valid).unwrap(), r);
    for mutation in 0..4 {
        let mut broken = valid.clone();
        match mutation {
            0 => broken.refs.get_mut("/prompt").unwrap()[0].ordinal = 1,
            1 => {
                broken.refs.get_mut("/prompt").unwrap().clear();
            }
            2 => {
                let chunk = broken.refs["/prompt"][0].clone();
                broken.refs.get_mut("/prompt").unwrap().push(chunk);
            }
            _ => {
                let mut value: Value =
                    serde_json::from_str(broken.inline.as_ref().unwrap()).unwrap();
                value["prompt"]["$blob"]["chunks"] = 0.into();
                let bytes = serde_json::to_string(&value).unwrap();
                broken.bytes = bytes.len() as u64;
                broken.sha256 = BlobHash::of(bytes.as_bytes()).to_string();
                broken.inline = Some(bytes);
            }
        }
        assert!(
            matches!(
                decode_run(&store, &broken),
                Err(FormatError::Invalid {
                    check: "prompt metadata",
                    ..
                })
            ),
            "mutation {mutation}"
        );
    }
}
