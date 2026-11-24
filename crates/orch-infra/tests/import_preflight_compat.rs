//! Synthetic compatibility vectors through the actual read-only source walker.
use orch_infra::import::preflight::{Options, Report, SystemProbe, preflight};
use serde_json::{Value, json};
use std::{fs, path::Path, sync::Arc};

fn source() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir(dir.path().join(".orchestry")).unwrap();
    dir
}
fn put(root: &Path, path: &str, bytes: impl AsRef<[u8]>) {
    let path = root.join(".orchestry").join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}
async fn run(root: &Path) -> Report {
    preflight(root.to_owned(), Options::default(), Arc::new(SystemProbe))
        .await
        .unwrap()
}
fn entity(store: &str, id: &str) -> Value {
    let time = "2025-01-01T00:00:00.000Z";
    match store {
        "tasks" => {
            json!({"id":id,"title":id,"description":"d","status":"todo","priority":1,"created_at":time,"updated_at":time,"attempts":0,"max_attempts":3})
        }
        "agents" => {
            json!({"id":id,"name":id,"adapter":"shell","role":"r","status":"idle","created_at":time,"updated_at":time,"config":{},"stats":{"tasks_completed":0,"tasks_failed":0,"total_runs":0,"total_runtime_ms":0}})
        }
        "goals" => {
            json!({"id":id,"title":id,"description":"d","status":"active","created_at":time,"updated_at":time})
        }
        "messages" => {
            json!({"id":id,"channel":"broadcast","from_agent_id":"cli","to_agent_id":null,"subject":id,"body":"m","created_at":time,"status":"pending"})
        }
        "context" => json!({"key":id,"value":id,"created_at":time,"updated_at":time}),
        _ => unreachable!(),
    }
}
async fn legacy_index(store: &str, dir: std::path::PathBuf) -> Value {
    use orch_infra::storage::{IndexManager, index::ItemFormat};
    let format = if matches!(store, "messages" | "context") {
        ItemFormat::Json
    } else {
        ItemFormat::Yaml
    };
    macro_rules! read {
        ($entity:ty) => {
            serde_json::to_value(
                IndexManager::<$entity>::new(dir, format)
                    .read_index()
                    .await
                    .unwrap(),
            )
            .unwrap()
        };
    }
    match store {
        "tasks" => read!(orch_domain::Task),
        "agents" => read!(orch_domain::Agent),
        "goals" => read!(orch_domain::Goal),
        "messages" => read!(orch_domain::Message),
        "context" => read!(orch_core::storage::ContextEntry),
        _ => unreachable!(),
    }
}

const STORES: [&str; 5] = ["tasks", "agents", "goals", "messages", "context"];
fn populate(root: &Path, store: &str) {
    for id in ["c", "a", "b"] {
        let ext = if matches!(store, "messages" | "context") {
            "json"
        } else {
            "yml"
        };
        put(
            root,
            &format!("{store}/{id}.{ext}"),
            serde_json::to_vec(&entity(store, id)).unwrap(),
        );
    }
}

#[tokio::test]
async fn config_empty_null_and_non_mapping_diagnostics_match_the_contract() {
    for (input, rule, legacy) in [
        ("", "documents", true),
        (" \n  \n", "documents", true),
        ("# synthetic config\n", "documents", true),
        ("null\n", "unreadable", true),
        ("~\n", "unreadable", true),
        ("---\n", "unreadable", true),
        ("42\n", "unreadable", true),
        ("[1]\n", "unreadable", true),
        ("\"x\"\n", "unreadable", true),
        ("\t\n", "unreadable", false),
        ("{}\n---\n{}\n", "documents", false),
    ] {
        let src = source();
        put(src.path(), "config.yml", input);
        let report = run(src.path()).await;
        assert_eq!(report.refusals.total(), 1, "{input:?}: {report:?}");
        let finding = &report.refusals.listed[0];
        assert_eq!((finding.path.as_str(), finding.rule), ("config.yml", rule));
        assert_eq!(
            finding.detail.contains("1.0 read"),
            legacy,
            "{input:?}: {finding:?}"
        );
        if input.is_empty() {
            assert!(finding.detail.contains("expected one document, found 0"));
        }
        assert_eq!(
            fs::read(src.path().join(".orchestry/config.yml")).unwrap(),
            input.as_bytes()
        );
    }
}

#[tokio::test]
async fn omitted_config_run_cap_uses_frozen_legacy_default_and_a_note() {
    for input in [
        None,
        Some("{}\n"),
        Some("project: {name: p}\n"),
        Some("scheduling: {max_concurrent_agents: 2}\n"),
    ] {
        let src = source();
        if let Some(input) = input {
            put(src.path(), "config.yml", input);
        }
        let report = run(src.path()).await;
        assert!(!report.refused(), "{report:?}");
        let explicit = input.is_some_and(|s| s.contains("max_concurrent_agents"));
        let value = serde_json::to_value(&report).unwrap();
        assert_eq!(value["run_cap"], if explicit { json!(2) } else { json!(6) });
        let notes: Vec<_> = report
            .notes
            .listed
            .iter()
            .filter(|f| f.rule == "config_default")
            .collect();
        assert_eq!(notes.len(), usize::from(!explicit));
        if !explicit {
            assert!(notes[0].detail.contains("1.0"));
        }
    }
}

#[tokio::test]
async fn whole_array_bad_entry_precedes_duplicates_in_all_five_directories() {
    for store in STORES {
        let key = if store == "context" { "key" } else { "id" };
        for input in [
            json!([{key:"b"},{key:"b"},0]),
            json!([0,{key:"b"},{key:"b"}]),
            json!([{}]),
            json!([{key:1}]),
        ] {
            let src = source();
            populate(src.path(), store);
            put(
                src.path(),
                &format!("{store}/_index.json"),
                serde_json::to_vec(&input).unwrap(),
            );
            let report = run(src.path()).await;
            assert_eq!(report.refusals.total(), 1, "{store}: {report:?}");
            let f = &report.refusals.listed[0];
            assert_eq!(f.rule, "index_entry");
            assert!(f.detail.contains("1.0 rebuilt"));
        }
    }
}

#[tokio::test]
async fn both_duplicate_shapes_and_missing_files_stop_in_all_five_directories() {
    for store in STORES {
        let key = if store == "context" { "key" } else { "id" };
        for input in [
            json!([{key:"b"},{key:"b"}]),
            json!([entity(store, "b"), entity(store, "b")]),
            json!([{key:"z"},{key:"z"}]),
        ] {
            let src = source();
            populate(src.path(), store);
            put(
                src.path(),
                &format!("{store}/_index.json"),
                serde_json::to_vec(&input).unwrap(),
            );
            let report = run(src.path()).await;
            assert_eq!(report.refusals.total(), 1, "{store}: {report:?}");
            let f = &report.refusals.listed[0];
            assert_eq!(f.rule, "index_duplicate");
            assert_eq!(f.observed.as_deref(), input[0][key].as_str());
            assert!(
                f.detail
                    .contains("only if every entry was a complete entity")
            );
            assert!(f.detail.contains("otherwise rebuilt"));
            // Legacy readers may publish; give them an independently created copy.
            let copy = source();
            populate(copy.path(), store);
            put(
                copy.path(),
                &format!("{store}/_index.json"),
                input.to_string(),
            );
            let read = legacy_index(store, copy.path().join(format!(".orchestry/{store}"))).await;
            let ids: Vec<_> = read
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v[key].as_str().unwrap())
                .collect();
            if input[0].get("status").is_some()
                || (store == "context" && input[0].get("value").is_some())
            {
                assert_eq!(ids, ["b", "b"], "{store}: complete duplicates kept by 1.0");
            } else {
                let mut ids = ids;
                ids.sort();
                assert_eq!(ids, ["a", "b", "c"], "{store}: identities rebuilt by 1.0");
            }
            assert_eq!(
                fs::read(src.path().join(format!(".orchestry/{store}/_index.json"))).unwrap(),
                serde_json::to_vec(&input).unwrap()
            );
        }
    }
}

#[tokio::test]
async fn index_order_and_named_problems_use_files_and_never_open_index_identities() {
    for store in STORES {
        let key = if store == "context" { "key" } else { "id" };
        for (input, expected) in [
            (None, vec!["a", "b", "c"]),
            (Some("[".to_owned()), vec!["a", "b", "c"]),
            (Some("{}".to_owned()), vec!["a", "b", "c"]),
            (Some("null".to_owned()), vec!["a", "b", "c"]),
            (Some("[]".to_owned()), vec!["a", "b", "c"]),
            (
                Some(json!([{key:"b"},{key:"../x"},{key:"A"},{key:"a"}]).to_string()),
                vec!["b", "a", "c"],
            ),
        ] {
            let src = source();
            populate(src.path(), store);
            if let Some(input) = &input {
                put(src.path(), &format!("{store}/_index.json"), input);
            }
            let report = run(src.path()).await;
            assert!(!report.refused(), "{store}: {report:?}");
            let value = serde_json::to_value(&report).unwrap();
            let order: Vec<_> = value["order"]["listed"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v["id"].as_str().unwrap())
                .collect();
            assert_eq!(order, expected, "{store}: {report:?}");
            let unreadable = input
                .as_deref()
                .is_none_or(|s| matches!(s, "[" | "{}" | "null"));
            assert_eq!(
                report
                    .notes
                    .listed
                    .iter()
                    .filter(|f| f.rule == "index_unreadable")
                    .count(),
                usize::from(unreadable)
            );
            if input.as_deref().is_some_and(|s| s.contains("../x")) {
                assert!(report.notes.listed.iter().any(
                    |f| f.rule == "index_missing_file" && f.observed.as_deref() == Some("../x")
                ));
                assert!(
                    report
                        .notes
                        .listed
                        .iter()
                        .any(|f| f.rule == "index_differs" && f.observed.as_deref() == Some("b"))
                );
            }
        }
    }
}

#[tokio::test]
async fn unknown_config_is_inert_bounded_and_values_are_not_reported() {
    let src = source();
    put(
        src.path(),
        "config.yml",
        "x_extension: {secret: hidden-value}\nproject: {name: p, x_note: v}\nprompt: {system_template: hello}\n",
    );
    let report = run(src.path()).await;
    assert!(!report.refused(), "{report:?}");
    let text = serde_json::to_string(&report).unwrap();
    assert!(!text.contains("hidden-value"));
    assert!(!text.contains("hello"));
    assert_eq!(
        report
            .notes
            .listed
            .iter()
            .filter(|f| f.rule == "unknown_config")
            .count(),
        2
    );
    let overhead = json!({"x_extension":""}).to_string().len();
    put(
        src.path(),
        "config.yml",
        json!({"x_extension":"x".repeat(65_536-overhead)}).to_string(),
    );
    let at = run(src.path()).await;
    assert!(!at.refused(), "{at:?}");
    assert_eq!(at.legacy_config_bytes, 65_536);
    let value = json!({"x_extension":"x".repeat(65_536)});
    put(src.path(), "config.yml", value.to_string());
    let report = run(src.path()).await;
    assert_eq!(report.refusals.listed[0].rule, "legacy_config_bytes");
    assert!(
        report.refusals.listed[0]
            .observed
            .as_ref()
            .unwrap()
            .parse::<u64>()
            .unwrap()
            > 65_536
    );
}

#[tokio::test]
async fn auto_agents_require_owner_confirmation_without_granting_policy() {
    for (policy, project_policy, expected) in [
        (None, "auto", true),
        (Some("auto"), "manual", true),
        (None, "manual", false),
        (Some("manual"), "auto", false),
    ] {
        let src = source();
        let mut agent = entity("agents", "a");
        if let Some(policy) = policy {
            agent["config"]["approval_policy"] = json!(policy);
        }
        put(src.path(), "agents/a.yml", agent.to_string());
        put(
            src.path(),
            "config.yml",
            format!("defaults: {{agent: {{approval_policy: {project_policy}}}}}\n"),
        );
        let report = run(src.path()).await;
        assert!(!report.refused(), "{report:?}");
        let value = serde_json::to_value(&report).unwrap();
        assert_eq!(value["preapproval_requires_confirmation"], json!(expected));
        assert!(value.get("preapproval").is_none());
    }
}

#[tokio::test]
async fn budgets_and_file_kind_stops_never_become_index_unreadable() {
    use std::os::unix::fs::symlink;
    for store in STORES {
        for (input, rule) in [
            (format!("{}0{}", "[".repeat(65), "]".repeat(65)), "depth"),
            (format!("[{}]", "0,".repeat(1_000_000) + "0"), "nodes"),
            (" ".repeat(16_777_217), "bytes"),
        ] {
            let src = source();
            populate(src.path(), store);
            put(src.path(), &format!("{store}/_index.json"), input);
            let report = run(src.path()).await;
            assert_eq!(report.refusals.listed[0].rule, rule);
            assert!(
                !report
                    .notes
                    .listed
                    .iter()
                    .any(|f| f.rule == "index_unreadable")
            );
        }
        for kind in ["symlink", "fifo", "hardlink"] {
            let src = source();
            populate(src.path(), store);
            let canary = src.path().join("canary");
            fs::write(&canary, "[").unwrap();
            let index = src.path().join(format!(".orchestry/{store}/_index.json"));
            match kind {
                "symlink" => symlink(&canary, &index).unwrap(),
                "hardlink" => fs::hard_link(&canary, &index).unwrap(),
                "fifo" => nix::unistd::mkfifo(&index, nix::sys::stat::Mode::S_IRUSR).unwrap(),
                _ => unreachable!(),
            }
            let report = run(src.path()).await;
            assert_eq!(report.refusals.listed[0].rule, "file_kind");
            assert!(
                !report
                    .notes
                    .listed
                    .iter()
                    .any(|f| f.rule == "index_unreadable")
            );
            assert_eq!(fs::read(&canary).unwrap(), b"[");
        }
    }
    let src = source();
    put(
        src.path(),
        "config.yml",
        format!("#{}", "x".repeat(1_048_576)),
    );
    assert_eq!(run(src.path()).await.refusals.listed[0].rule, "bytes");
}

#[tokio::test]
async fn empty_entity_streams_keep_their_document_refusal_without_config_wording() {
    use orch_core::storage::{AgentStore, GoalStore, TaskStore, TeamStore};
    use orch_infra::storage::{FileAgentStore, FileGoalStore, FileTaskStore, FileTeamStore, Paths};
    for store in ["tasks", "agents", "goals", "teams"] {
        let copy = source();
        put(copy.path(), &format!("{store}/a.yml"), "");
        let paths = Paths::new(copy.path());
        let refused = match store {
            "tasks" => FileTaskStore::new(paths).get("a").await.is_err(),
            "agents" => FileAgentStore::new(paths).get("a").await.is_err(),
            "goals" => FileGoalStore::new(paths).get("a").await.is_err(),
            "teams" => FileTeamStore::new(paths).get("a").await.is_err(),
            _ => unreachable!(),
        };
        assert!(refused, "1.0 also refuses empty {store}");
        let src = source();
        put(src.path(), &format!("{store}/a.yml"), "");
        let report = run(src.path()).await;
        let f = &report.refusals.listed[0];
        assert_eq!(f.rule, "documents");
        assert_eq!(f.observed.as_deref(), Some("0"));
        assert!(f.detail.contains("expected one document, found 0"));
        assert!(!f.detail.contains("1.0 read"));
    }
}

#[tokio::test]
async fn hostile_duplicate_identity_is_escaped_and_cut_without_changing_class() {
    let src = source();
    let id = format!("\n\u{1b}[31m{}", "é".repeat(2000));
    put(
        src.path(),
        "context/_index.json",
        json!([{"key":id},{"key":id}]).to_string(),
    );
    let report = run(src.path()).await;
    let f = &report.refusals.listed[0];
    assert_eq!(f.rule, "index_duplicate");
    let observed = f.observed.as_ref().unwrap();
    assert!(!observed.contains('\n'));
    assert!(!observed.contains('\u{1b}'));
    assert!(observed.len() < 4000);
}

#[tokio::test]
async fn order_and_notes_are_capped_without_changing_counts() {
    let src = source();
    for n in 0..1003 {
        put(
            src.path(),
            &format!("context/k{n:04}.json"),
            entity("context", &format!("k{n:04}")).to_string(),
        );
    }
    put(src.path(), "context/_index.json", "[]");
    let report = run(src.path()).await;
    assert!(!report.refused(), "{report:?}");
    assert_eq!(report.counts.context, 1003);
    let value = serde_json::to_value(&report).unwrap();
    assert_eq!(value["order"]["listed"].as_array().unwrap().len(), 1000);
    assert_eq!(value["order"]["more"], json!(3));
    assert_eq!(report.notes.listed.len(), 1000);
    assert_eq!(report.notes.more, 4); // 3 unlisted overflow + default note
}

#[tokio::test]
async fn typed_json_documents_still_refuse_duplicate_identity_members() {
    for store in ["messages", "context"] {
        let key = if store == "context" { "key" } else { "id" };
        let value = entity(store, "a").to_string();
        let input = format!("{{\"{key}\":\"a\",{}", &value[1..]);
        let src = source();
        put(src.path(), &format!("{store}/a.json"), input);
        let report = run(src.path()).await;
        assert_eq!(report.refusals.listed[0].rule, "unreadable");
        assert!(report.refusals.listed[0].detail.contains("duplicate field"));
    }
}

#[tokio::test]
async fn typed_yaml_entities_keep_ignored_yaml_only_fields() {
    let src = source();
    let mut agent = entity("agents", "a");
    agent["config"]["approval_policy"] = json!("auto");
    let input = format!(
        "{}\nx_extension: {{[a, b]: ignored}}\n",
        serde_yaml_ng::to_string(&agent).unwrap()
    );
    put(src.path(), "agents/a.yml", input);
    put(
        src.path(),
        "config.yml",
        "defaults: {agent: {approval_policy: manual}}\n",
    );
    let report = run(src.path()).await;
    assert!(!report.refused(), "{report:?}");
    assert_eq!(report.counts.agents, 1);
    assert!(
        serde_json::to_value(&report).unwrap()["preapproval_requires_confirmation"]
            .as_bool()
            .unwrap()
    );
}

#[tokio::test]
async fn an_unreadable_entity_is_not_reported_as_a_missing_file() {
    let src = source();
    put(src.path(), "tasks/a.yml", "null\n");
    put(src.path(), "tasks/_index.json", r#"[{"id":"a"}]"#);
    let report = run(src.path()).await;
    assert!(report.refused());
    assert!(
        !report
            .notes
            .listed
            .iter()
            .any(|f| f.rule == "index_missing_file")
    );
}
