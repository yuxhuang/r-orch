//! Candidate regressions: the report preview is never a load manifest.
use super::*;
use orch_core::store::{Entity, EntityKind};
use std::fs;

async fn load(
    prepared: Arc<PreparedImport>,
    index: usize,
) -> Result<super::super::ImportRow, PreflightError> {
    tokio::task::spawn_blocking(move || prepared.load_row(index))
        .await
        .unwrap()
}

#[tokio::test]
async fn complete_order_and_exact_names_survive_the_report_cutoff() {
    let root = tempfile::tempdir().unwrap();
    let context = root.path().join(".orchestry/context");
    fs::create_dir_all(&context).unwrap();
    let mut ids: Vec<_> = (0..1002).map(|i| format!("key{i:04}")).collect();
    ids.push("x".repeat(240));
    for id in &ids {
        fs::write(context.join(format!("{id}.json")), serde_json::json!({
            "key": id, "value": "file value", "created_at": "2025-01-01T00:00:00.000Z", "updated_at": "2025-01-01T00:00:00.000Z"
        }).to_string()).unwrap();
    }
    ids.reverse();
    fs::write(
        context.join("_index.json"),
        serde_json::to_vec(
            &ids.iter()
                .map(|id| serde_json::json!({"key": id}))
                .collect::<Vec<_>>(),
        )
        .unwrap(),
    )
    .unwrap();
    let prepared = Arc::new(
        prepare(
            root.path().to_owned(),
            Options::default(),
            Arc::new(SystemProbe),
        )
        .await
        .unwrap(),
    );
    assert!(!prepared.report.refused());
    assert_eq!(prepared.report.order.listed.len(), REPORT_LIMIT);
    assert_eq!(prepared.report.order.more, 3);
    let inventory = prepared.inventory();
    for (index, id) in ids.iter().enumerate() {
        assert_eq!(
            inventory[&(EntityKind::Context, id.clone())].1,
            Some(index as u64 + 1)
        );
        let row = load(prepared.clone(), index).await.unwrap();
        assert_eq!(row.entity.id(), id);
        assert!(matches!(row.entity, Entity::Context(ref value) if value.value == "file value"));
    }
}

#[tokio::test]
async fn known_settings_preserve_types_and_extensions_cannot_smuggle_policy() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join(".orchestry")).unwrap();
    let path = root.path().join(".orchestry/config.yml");
    fs::write(&path, "project: {name: synthetic}\nscheduling: {max_concurrent_agents: 2}\npreapproval: true\nrun_cap: 99\nprompt: {template: custom, extension: ignored}\n").unwrap();
    let prepared = Arc::new(
        prepare(
            root.path().to_owned(),
            Options::default(),
            Arc::new(SystemProbe),
        )
        .await
        .unwrap(),
    );
    assert!(!prepared.report.refused());
    let mut settings = std::collections::BTreeMap::new();
    for index in 0..prepared.row_count() {
        let Entity::Config(row) = load(prepared.clone(), index).await.unwrap().entity else {
            panic!("config row");
        };
        settings.insert(
            row.key,
            serde_json::from_str::<serde_json::Value>(&row.value).unwrap(),
        );
    }
    assert_eq!(settings["run_cap"], 2);
    assert_eq!(settings["project.name"], "synthetic");
    assert_eq!(settings["defaults.agent.max_turns"], 50);
    assert_eq!(settings["prompt.template"], "custom");
    assert!(!settings.contains_key("preapproval"));
    assert!(!settings.contains_key("scheduling.max_concurrent_agents"));
    assert!(!settings.contains_key("prompt.extension"));
    assert_eq!(
        settings["legacy.config"],
        serde_json::json!({"preapproval":true,"run_cap":99,"prompt":{"extension":"ignored"}})
    );
    fs::write(path, "{}\n").unwrap();
    assert!(load(prepared, 0).await.is_err());
}

#[tokio::test]
async fn a_missing_config_cannot_become_present_after_preflight() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join(".orchestry")).unwrap();
    let prepared = Arc::new(
        prepare(
            root.path().to_owned(),
            Options::default(),
            Arc::new(SystemProbe),
        )
        .await
        .unwrap(),
    );
    let settings = prepared.inventory();
    assert!(settings.contains_key(&(EntityKind::Config, "run_cap".into())));
    fs::write(root.path().join(".orchestry/config.yml"), "{}\n").unwrap();
    assert!(load(prepared, 0).await.is_err());
}

#[tokio::test]
async fn native_load_refuses_same_length_changes_after_prepare() {
    let root = tempfile::tempdir().unwrap();
    let tasks = root.path().join(".orchestry/tasks");
    fs::create_dir_all(&tasks).unwrap();
    let path = tasks.join("retained.yml");
    let original = "id: retained\ntitle: original\ndescription: source file\nstatus: todo\npriority: 1\ncreated_at: '2025-01-01T00:00:00.000Z'\nupdated_at: '2025-01-01T00:00:00.000Z'\nattempts: 0\nmax_attempts: 3\n";
    fs::write(&path, original).unwrap();
    let prepared = Arc::new(
        prepare(
            root.path().to_owned(),
            Options::default(),
            Arc::new(SystemProbe),
        )
        .await
        .unwrap(),
    );
    assert!(!prepared.report.refused());
    let row = load(prepared.clone(), 0).await.unwrap();
    assert!(matches!(row.entity, Entity::Task(ref task) if task.title == "original"));
    let changed = original.replace("original", "modified");
    assert_eq!(changed.len(), original.len());
    fs::write(&path, changed).unwrap();
    assert!(
        matches!(load(prepared, 0).await, Err(PreflightError::Source(detail)) if detail == "entity changed since preflight")
    );
}

#[tokio::test]
async fn missing_indexed_file_and_unlisted_files_resume_with_dense_ranks() {
    use crate::data_dir::{DataDir, Holder, LockKind};
    use crate::import::ImportBatch;
    use crate::sqlite::{SqliteStorage, StorageOptions, WriteOutcome};
    use orch_core::store::{Caller, Principal, Revision, Storage};

    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    let tasks = source.join(".orchestry/tasks");
    fs::create_dir_all(&tasks).unwrap();
    for id in ["zulu", "alpha", "unlisted_z", "unlisted_a"] {
        fs::write(tasks.join(format!("{id}.yml")), format!("id: {id}\ntitle: synthetic\ndescription: source file\nstatus: todo\npriority: 1\ncreated_at: '2025-01-01T00:00:00.000Z'\nupdated_at: '2025-01-01T00:00:00.000Z'\nattempts: 0\nmax_attempts: 3\n")).unwrap();
    }
    fs::write(
        tasks.join("_index.json"),
        r#"[{"id":"zulu"},{"id":"missing"},{"id":"alpha"}]"#,
    )
    .unwrap();
    let prepared = Arc::new(
        prepare(source.clone(), Options::default(), Arc::new(SystemProbe))
            .await
            .unwrap(),
    );
    assert!(!prepared.report.refused());
    let caller = Caller {
        project: "synthetic".into(),
        principal: Principal::Owner {
            identity: "local_owner".into(),
        },
    };
    let data = Arc::new(
        DataDir::acquire(
            &root.path().join("destination"),
            &Holder {
                kind: LockKind::Import,
                pid: std::process::id(),
                started_at: "synthetic".into(),
                binary_version: "test".into(),
                executable: "orch import".into(),
            },
        )
        .unwrap(),
    );
    let storage = SqliteStorage::open(data.clone(), StorageOptions::default()).unwrap();
    let ids = ["zulu", "alpha", "unlisted_a", "unlisted_z"];
    for (index, id) in ids.iter().enumerate() {
        let row = load(prepared.clone(), index).await.unwrap();
        assert_eq!(row.entity.id(), *id);
        let attempt = storage
            .import_batch(
                caller.clone(),
                ImportBatch {
                    source: source.to_string_lossy().into_owned(),
                    rows: vec![row],
                },
            )
            .await
            .unwrap();
        assert_eq!(attempt.outcome, WriteOutcome::Committed);
        attempt.result.unwrap();
    }
    drop(prepared);
    let resumed = Arc::new(
        prepare(source.clone(), Options::default(), Arc::new(SystemProbe))
            .await
            .unwrap(),
    );
    assert!(!resumed.report.refused());
    let inventory = resumed.inventory();
    assert!(!inventory.contains_key(&(EntityKind::Task, "missing".into())));
    // Use the actual preflight inventory, so raw index gaps cannot pass resume.
    let checked = storage
        .check_import_source(
            caller.clone(),
            source.to_string_lossy().into_owned(),
            inventory.clone(),
        )
        .await
        .unwrap();
    assert_eq!(checked.outcome, WriteOutcome::Committed);
    checked.result.unwrap();
    let view = storage.read(caller.clone()).await.unwrap();
    let mut before = Vec::new();
    for (index, id) in ids.iter().enumerate() {
        assert_eq!(
            inventory[&(EntityKind::Task, (*id).into())].1,
            Some(index as u64 + 1)
        );
        let record = view.get(EntityKind::Task, id).await.unwrap().unwrap();
        assert_eq!(record.position, index as u64 + 1);
        assert_eq!(record.revision, Revision::ONE);
        before.push(record);
    }
    assert!(
        view.get(EntityKind::Task, "missing")
            .await
            .unwrap()
            .is_none()
    );
    drop(view);
    for index in 0..ids.len() {
        let row = load(resumed.clone(), index).await.unwrap();
        let attempt = storage
            .import_batch(
                caller.clone(),
                ImportBatch {
                    source: source.to_string_lossy().into_owned(),
                    rows: vec![row],
                },
            )
            .await
            .unwrap();
        assert_eq!(attempt.outcome, WriteOutcome::Committed);
        attempt.result.unwrap();
    }
    let view = storage.read(caller).await.unwrap();
    for (id, before) in ids.iter().zip(before) {
        assert_eq!(
            view.get(EntityKind::Task, id).await.unwrap().unwrap(),
            before
        );
    }
    drop(view);
    storage.close().await.unwrap();
}
