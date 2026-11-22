use super::document::{Limits, check_json, check_yaml, read_bounded};

#[test]
fn budgets_refuse_one_over() {
    let limits = Limits {
        bytes: 8,
        depth: 2,
        nodes: 3,
        aliases: 1,
        expanded_bytes: 4,
    };
    assert!(check_json(b"[[0]]", limits).is_ok());
    assert_eq!(check_json(b"[[[0]]]", limits).unwrap_err().rule, "depth");
    assert!(check_json(b"[0,0]", limits).is_ok());
    assert_eq!(check_json(b"[0,0,0]", limits).unwrap_err().rule, "nodes");
    assert!(check_yaml(b"abcd", limits).is_ok());
    assert_eq!(
        check_yaml(b"abcde", limits).unwrap_err().rule,
        "expanded_bytes"
    );
    assert_eq!(
        read_bounded(&b"123456789"[..], 8).unwrap_err().rule,
        "bytes"
    );
}

#[test]
fn yaml_aliases_tags_cycles_documents_and_expanded_depth_are_checked() {
    let limits = Limits {
        depth: 3,
        aliases: 1,
        ..Limits::YAML
    };
    assert!(check_yaml(b"a: &x [0]\nb: *x", limits).is_ok());
    assert_eq!(
        check_yaml(b"a: &x [0]\nb: [*x, *x]", limits)
            .unwrap_err()
            .rule,
        "aliases"
    );
    assert_eq!(
        check_yaml(b"a: &x [0]\nb: [[*x]]", limits)
            .unwrap_err()
            .rule,
        "depth"
    );
    assert_eq!(
        check_yaml(b"a: &x [*x]", limits).unwrap_err().rule,
        "alias_cycle"
    );
    for input in [
        "!x a",
        "!!str a",
        "! a",
        "!x []",
        "!!map {}",
        "!!binary YWJj",
    ] {
        assert_eq!(
            check_yaml(input.as_bytes(), limits).unwrap_err().rule,
            "tag",
            "{input}"
        );
    }
    assert_eq!(
        check_yaml(b"---\na\n---\nb", limits).unwrap_err().rule,
        "documents"
    );
    assert_eq!(check_yaml(b"", limits).unwrap_err().rule, "documents");
}

#[test]
fn yaml_alias_and_merge_expansions_share_the_node_and_byte_budgets() {
    let input = b"a: &x {k: v}\nb: {<<: *x}\nc: *x";
    assert!(
        check_yaml(
            input,
            Limits {
                nodes: 15,
                expanded_bytes: 11,
                ..Limits::YAML
            }
        )
        .is_ok()
    );
    assert_eq!(
        check_yaml(
            input,
            Limits {
                nodes: 14,
                ..Limits::YAML
            }
        )
        .unwrap_err()
        .rule,
        "expanded_nodes"
    );
    assert_eq!(
        check_yaml(
            input,
            Limits {
                expanded_bytes: 10,
                ..Limits::YAML
            }
        )
        .unwrap_err()
        .rule,
        "expanded_bytes"
    );
}

#[tokio::test]
async fn merge_values_match_the_guarded_legacy_reader() {
    let input = b"base: &x {a: 1, b: 2}\nother: &y {c: 3}\nmerged: {<<: [*x, *y], b: 4}";
    let actual: serde_yaml_ng::Value = super::document::parse_yaml(&input[..], Limits::YAML)
        .await
        .unwrap();
    let legacy =
        orch_core::definition_yaml::parse_yaml_bytes(std::path::Path::new("<synthetic>"), input)
            .await
            .unwrap();
    assert_eq!(actual, legacy);
    assert_eq!(actual["merged"]["b"].as_i64(), Some(4));
}

#[test]
fn json_counts_values_not_member_names_and_rejects_trailing_data() {
    let limits = Limits {
        nodes: 2,
        depth: 1,
        ..Limits::JSON
    };
    assert!(check_json(br#"{"key":0}"#, limits).is_ok());
    assert_eq!(
        check_json(br#"{"key":0,"key":1}"#, limits)
            .unwrap_err()
            .rule,
        "nodes"
    );
    for input in ["0 1", "{", "[NaN]", "\"\\uD800\""] {
        assert_eq!(
            check_json(input.as_bytes(), limits).unwrap_err().rule,
            "parse"
        );
    }
    assert!(check_json(&[255], limits).is_err());
}

#[test]
fn endless_reader_stops_at_bound_plus_one() {
    use std::io::Read;
    let mut endless = std::io::repeat(b'x');
    let mut counted = (&mut endless).take(100);
    assert_eq!(read_bounded(&mut counted, 8).unwrap_err().observed, 9);
    assert_eq!(counted.limit(), 91);
}

#[test]
fn depth_bombs_are_refused_on_a_small_stack() {
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            let bomb = format!("{}0{}", "[".repeat(20_000), "]".repeat(20_000));
            assert_eq!(
                check_yaml(bomb.as_bytes(), Limits::YAML).unwrap_err().rule,
                "depth"
            );
            assert_eq!(
                check_json(bomb.as_bytes(), Limits::JSON).unwrap_err().rule,
                "depth"
            );
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn exponential_alias_bomb_is_refused_before_building() {
    let mut input = "a: &a [x]\n".to_owned();
    let mut previous = "a".to_owned();
    for i in 0..30 {
        let next = format!("a{i}");
        input.push_str(&format!("{next}: &{next} [*{previous}, *{previous}]\n"));
        previous = next;
    }
    assert_eq!(
        check_yaml(input.as_bytes(), Limits::YAML).unwrap_err().rule,
        "expanded_nodes"
    );
}

#[test]
fn event_stream_retains_order_and_coalesces_unusable_lines() {
    use super::event::{EventItem, EventStream};
    use serde_json::json;
    let lines = b"0\nxxxxxxxxxxxxxxxx\n\xff\n[[[0]]]\nnot-json\n1\n\n2";
    let stream = EventStream::<_, serde_json::Value>::new(
        &lines[..],
        Limits {
            bytes: 8,
            depth: 2,
            ..Limits::EVENT
        },
    );
    let actual: Vec<_> = stream.collect::<Result<_, _>>().unwrap();
    assert_eq!(
        actual,
        vec![
            EventItem::Event {
                line: 1,
                value: json!(0)
            },
            EventItem::Gap { first: 2, last: 5 },
            EventItem::Event {
                line: 6,
                value: json!(1)
            },
            EventItem::Gap { first: 7, last: 7 },
            EventItem::Event {
                line: 8,
                value: json!(2)
            }
        ]
    );
}

#[test]
fn event_line_limit_excludes_the_newline_and_drains_to_eof() {
    use super::event::{EventItem, EventStream};
    let lines = b"1234\n12345\n0";
    let actual: Vec<_> = EventStream::<_, u64>::new(
        &lines[..],
        Limits {
            bytes: 4,
            ..Limits::EVENT
        },
    )
    .collect::<Result<_, _>>()
    .unwrap();
    assert_eq!(
        actual,
        vec![
            EventItem::Event {
                line: 1,
                value: 1234
            },
            EventItem::Gap { first: 2, last: 2 },
            EventItem::Event { line: 3, value: 0 }
        ]
    );
    let long = std::io::Read::take(std::io::repeat(b'x'), 256 * 1024 * 1024);
    assert_eq!(
        EventStream::<_, u64>::new(long, Limits::EVENT)
            .next()
            .unwrap()
            .unwrap(),
        EventItem::Gap { first: 1, last: 1 }
    );
}

#[test]
fn io_errors_are_not_event_gaps() {
    struct Broken;
    impl std::io::Read for Broken {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("synthetic failure"))
        }
    }
    let mut stream = super::event::EventStream::<_, u64>::new(Broken, Limits::EVENT);
    assert_eq!(stream.next().unwrap().unwrap_err().rule, "io");
    assert!(stream.next().is_none());
}

#[test]
fn index_content_comes_from_files_and_order_comes_from_index() {
    use super::index::{IndexProblem, reconcile};
    use serde_json::json;
    let files = [
        ("a".into(), json!({"id":"a", "title":"file-a"})),
        ("b".into(), json!({"id":"b", "title":"file-b"})),
        ("c".into(), json!({"id":"c"})),
    ]
    .into();
    let index = json!([{"id":"b", "title":"stale"}, {"id":"absent"}, {"id":"a", "title":"file-a"}]);
    let (rows, problems) = reconcile(Some(&index), files, "id").unwrap();
    assert_eq!(
        rows.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(),
        ["b", "a", "c"]
    );
    assert_eq!(rows[0].1["title"], "file-b");
    assert_eq!(
        problems,
        vec![
            IndexProblem::Differs("b".into()),
            IndexProblem::MissingFile("absent".into()),
            IndexProblem::Unlisted("c".into())
        ]
    );
}

#[test]
fn absent_index_falls_back_to_id_order_and_bad_entries_refuse() {
    use super::index::{IndexProblem, reconcile};
    use serde_json::json;
    let files = [
        ("c".into(), json!({"id":"c"})),
        ("a".into(), json!({"id":"a"})),
    ]
    .into();
    let (rows, problems) = reconcile(None, files, "id").unwrap();
    assert_eq!(
        rows.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(),
        ["a", "c"]
    );
    assert_eq!(problems, [IndexProblem::Unreadable]);
    for index in [json!([{}]), json!([{"id":"a"}, {"id":"a"}])] {
        assert_eq!(
            reconcile(Some(&index), Default::default(), "id")
                .unwrap_err()
                .rule,
            "index_entry"
        );
    }
}

#[tokio::test]
async fn reviewed_synthetic_documents_parse_with_the_real_legacy_types() {
    use super::document::{parse_json, parse_yaml};
    use orch_domain::{agent::Agent, state::OrchestratorState, task::Task};
    let _: Task = parse_yaml(&include_bytes!("fixtures/task.yml")[..], Limits::YAML)
        .await
        .unwrap();
    let _: Agent = parse_yaml(&include_bytes!("fixtures/agent.yml")[..], Limits::YAML)
        .await
        .unwrap();
    let config = include_bytes!("fixtures/config.yml");
    let _: serde_yaml_ng::Value = parse_yaml(&config[..], Limits::YAML).await.unwrap();
    let _: OrchestratorState =
        parse_json(&include_bytes!("fixtures/state.json")[..], Limits::JSON).unwrap();
    let events = include_bytes!("fixtures/events.jsonl");
    let rows: Vec<_> =
        super::event::EventStream::<_, orch_domain::run::RunEvent>::new(&events[..], Limits::EVENT)
            .collect::<Result<_, _>>()
            .unwrap();
    assert_eq!(rows.len(), 4);
    assert!(
        rows.iter()
            .all(|row| matches!(row, super::event::EventItem::Event { .. }))
    );
}

#[tokio::test]
async fn merged_stale_index_source_matches_reviewed_expectations_without_writes() {
    use super::document::{parse_json, parse_yaml};
    use super::index::{IndexProblem, reconcile};
    use serde_json::Value;
    use std::{collections::BTreeMap, fs, path::Path};
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/storage-v1/index-disagrees/.orchestry/tasks");
    let before: BTreeMap<_, _> = fs::read_dir(&root)
        .unwrap()
        .map(|entry| {
            let path = entry.unwrap().path();
            let bytes = fs::read(&path).unwrap();
            (path, bytes)
        })
        .collect();
    let mut files = BTreeMap::new();
    let mut index = None;
    for (path, bytes) in &before {
        if path.extension().is_some_and(|extension| extension == "yml") {
            let value: Value = parse_yaml(&bytes[..], Limits::YAML).await.unwrap();
            files.insert(value["id"].as_str().unwrap().to_owned(), value);
        } else if path.file_name().unwrap() == "_index.json" {
            index = Some(parse_json::<Value>(&bytes[..], Limits::JSON).unwrap());
        }
    }
    let (rows, problems) = reconcile(index.as_ref(), files, "id").unwrap();
    let expected: Value =
        serde_json::from_slice(include_bytes!("fixtures/index-disagrees.json")).unwrap();
    assert_eq!(
        serde_json::json!(rows.iter().map(|(id, _)| id).collect::<Vec<_>>()),
        expected["importer_1_1"]["task_order"]
    );
    for (id, value) in rows {
        assert_eq!(
            value["title"],
            expected["importer_1_1"]["tasks"][&id]["title"]
        );
    }
    assert_eq!(
        problems,
        vec![
            IndexProblem::Differs("tsk_b".into()),
            IndexProblem::MissingFile("tsk_ghost".into()),
            IndexProblem::Unlisted("tsk_orphan".into())
        ]
    );
    for (path, bytes) in before {
        assert_eq!(fs::read(path).unwrap(), bytes);
    }
}

#[tokio::test]
async fn reviewed_absent_index_snapshot_uses_id_order_not_legacy_updated_order() {
    use serde_json::Value;
    let mut files = std::collections::BTreeMap::new();
    for bytes in [
        include_bytes!("fixtures/tsk_idx_a.yml").as_slice(),
        include_bytes!("fixtures/tsk_idx_b.yml").as_slice(),
        include_bytes!("fixtures/tsk_idx_c.yml").as_slice(),
    ] {
        let value: Value = super::document::parse_yaml(bytes, Limits::YAML)
            .await
            .unwrap();
        files.insert(value["id"].as_str().unwrap().to_owned(), value);
    }
    let (rows, _) = super::index::reconcile(None, files, "id").unwrap();
    let expected: Value =
        serde_json::from_slice(include_bytes!("fixtures/index-absent-update-order.json")).unwrap();
    let ids = serde_json::json!(rows.iter().map(|(id, _)| id).collect::<Vec<_>>());
    assert_eq!(ids, expected["importer_1_1"]["task_order"]);
    assert_ne!(ids, expected["legacy_1_0"]["list_order"]);
}

#[test]
fn json_scalar_syntax_and_structures_match_the_real_reader() {
    for input in [
        r#"{"a":[true,false,null,1.2e-3,"\\\"[]{}",{"b":"\uD83D\uDE00"}]}"#,
        "[]",
        "{}",
        "0",
        "\"\"",
        "[1,]",
        "{\"a\":}",
        "{1:0}",
        "[1 2]",
        "01",
        "false true",
        "{\"a\":1,}",
        "[}",
        "[[]",
        "{\"a\" 1}",
    ] {
        assert_eq!(
            check_json(input.as_bytes(), Limits::JSON).is_ok(),
            serde_json::from_str::<serde_json::Value>(input).is_ok(),
            "{input}"
        );
    }
}

#[test]
fn all_document_ceilings_accept_the_boundary_and_refuse_one_over() {
    for (limit, check) in [
        (
            Limits::YAML,
            check_yaml as fn(&[u8], Limits) -> Result<(), super::document::Refusal>,
        ),
        (Limits::JSON, check_json as fn(&[u8], Limits) -> _),
    ] {
        for depth in [0, 1, 64] {
            let input = format!("{}0{}", "[".repeat(depth), "]".repeat(depth));
            assert!(
                check(
                    input.as_bytes(),
                    Limits {
                        depth: depth as u64,
                        ..limit
                    }
                )
                .is_ok()
            );
            if depth > 0 {
                assert_eq!(
                    check(
                        input.as_bytes(),
                        Limits {
                            depth: depth as u64 - 1,
                            ..limit
                        }
                    )
                    .unwrap_err()
                    .rule,
                    "depth"
                );
            }
        }
        assert!(
            check(
                b"[0,0]",
                Limits {
                    bytes: 5,
                    nodes: 3,
                    ..limit
                }
            )
            .is_ok()
        );
        assert_eq!(
            check(b"[0,0]", Limits { bytes: 4, ..limit })
                .unwrap_err()
                .rule,
            "bytes"
        );
        let error = check(b"[0,0]", Limits { nodes: 2, ..limit }).unwrap_err();
        assert!(matches!(error.rule, "nodes" | "expanded_nodes"));
    }
    assert!(
        check_yaml(
            b"a: &x ab\nb: *x",
            Limits {
                aliases: 1,
                expanded_bytes: 6,
                ..Limits::YAML
            }
        )
        .is_ok()
    );
    assert_eq!(
        check_yaml(
            b"a: &x ab\nb: *x",
            Limits {
                aliases: 0,
                ..Limits::YAML
            }
        )
        .unwrap_err()
        .rule,
        "aliases"
    );
    assert_eq!(
        check_yaml(
            b"a: &x ab\nb: *x",
            Limits {
                expanded_bytes: 5,
                ..Limits::YAML
            }
        )
        .unwrap_err()
        .rule,
        "expanded_bytes"
    );
    assert_eq!(
        check_json(b"\"\xff\"", Limits::JSON).unwrap_err().rule,
        "parse"
    );
}

#[test]
fn iterative_json_structure_agrees_with_the_real_reader_on_synthetic_tokens() {
    let alphabet = b"[]{}:,\"\\ntf012-.e ab\r\n";
    let mut seed = 17_u64;
    for length in 0..24 {
        for _ in 0..500 {
            let mut input = Vec::new();
            for _ in 0..length {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                input.push(alphabet[((seed >> 32) as usize) % alphabet.len()]);
            }
            assert_eq!(
                check_json(&input, Limits::JSON).is_ok(),
                serde_json::from_slice::<serde_json::Value>(&input).is_ok(),
                "{input:?}"
            );
        }
    }
}
