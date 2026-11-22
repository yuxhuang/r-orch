//! What only a file reference can get wrong: the document on disk. The shared
//! scenarios in `orch-engine`'s `storage_conformance` cover the port's rules.

use std::path::Path;

use orch_core::store::events::{EventGap, SequencedEvent};
use orch_core::store::intents::{DONE_RETENTION_MS, FinishOutcome, IntentPayload};
use orch_core::store::leases::{ClaimOutcome, Lease, NewLease};
use orch_core::store::*;
use orch_core::test_support::{assert_hermetic, make_agent, make_task};
use orch_domain::{
    DEFAULT_TEAM_CONFIG, Message, MessageChannel, MessageStatus, RunEvent, RunEventType, Task,
    Team, TeamStatus,
};

use super::FileStorage;

fn owner(project: &str) -> Caller {
    Caller {
        project: project.into(),
        principal: Principal::Owner {
            identity: "owner".into(),
        },
    }
}

fn task(id: &str) -> Entity {
    Entity::Task(Box::new(Task {
        id: id.into(),
        ..make_task()
    }))
}

fn root() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    assert_hermetic(root.path());
    root
}

async fn create(s: &FileStorage, project: &str, id: &str) -> Record {
    let value = task(id);
    s.transaction(owner(project), Lane::Entities, move |u| u.create(value))
        .await
        .unwrap()
}

async fn get(s: &FileStorage, project: &str, id: &str) -> Option<Record> {
    s.read(owner(project))
        .await
        .unwrap()
        .get(EntityKind::Task, id)
        .await
        .unwrap()
}

fn documents(root: &Path) -> Vec<std::path::PathBuf> {
    let mut found = vec![];
    for dir in std::fs::read_dir(root).unwrap() {
        let dir = dir.unwrap().path();
        // The root also holds the daemon's lineage file once a restore ran.
        if !dir.is_dir() {
            continue;
        }
        for file in std::fs::read_dir(&dir).unwrap() {
            found.push(file.unwrap().path());
        }
    }
    found.sort();
    found
}

#[tokio::test]
async fn rows_revisions_and_positions_survive_a_new_instance() {
    let root = root();
    let first = FileStorage::new(root.path());
    create(&first, "p", "a").await;
    create(&first, "p", "b").await;
    first
        .write(
            owner("p"),
            Lane::Entities,
            Box::new(|u| {
                u.update(task("a"), Revision::ONE)?;
                u.delete(EntityKind::Task, "b", Revision::ONE)
            }),
        )
        .await
        .unwrap();
    drop(first);

    let second = FileStorage::new(root.path());
    let a = get(&second, "p", "a").await.unwrap();
    assert_eq!((a.revision.get(), a.position), (2, 1));
    assert_eq!(get(&second, "p", "b").await, None);
    // The counter outlives the deleted row, so its position is not handed out again.
    assert_eq!(create(&second, "p", "c").await.position, 3);
}

#[tokio::test]
async fn a_document_that_does_not_parse_is_refused_and_left_as_it_was() {
    let root = root();
    let s = FileStorage::new(root.path());
    create(&s, "p", "a").await;
    let [document] = documents(root.path()).try_into().unwrap();
    std::fs::write(&document, b"{ not json").unwrap();

    assert!(matches!(
        s.read(owner("p")).await,
        Err(StoreError::Backend(_))
    ));
    let write = s
        .write(
            owner("p"),
            Lane::Entities,
            Box::new(|u| u.create(task("b")).map(|_| ())),
        )
        .await;
    assert!(matches!(write, Err(StoreError::Backend(_))));
    assert_eq!(std::fs::read(&document).unwrap(), b"{ not json");
}

#[tokio::test]
async fn a_document_with_one_row_twice_is_refused() {
    let root = root();
    let s = FileStorage::new(root.path());
    create(&s, "p", "a").await;
    let [document] = documents(root.path()).try_into().unwrap();
    let mut value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&document).unwrap()).unwrap();
    let row = value["records"][0].clone();
    value["records"].as_array_mut().unwrap().push(row);
    std::fs::write(&document, serde_json::to_vec(&value).unwrap()).unwrap();
    assert!(matches!(
        s.read(owner("p")).await,
        Err(StoreError::Backend(_))
    ));
}

#[tokio::test]
async fn a_row_past_its_counter_is_refused() {
    let root = root();
    let s = FileStorage::new(root.path());
    create(&s, "p", "a").await;
    let [document] = documents(root.path()).try_into().unwrap();
    let mut value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&document).unwrap()).unwrap();
    value["counters"]["task"] = 0.into();
    std::fs::write(&document, serde_json::to_vec(&value).unwrap()).unwrap();
    assert!(matches!(
        s.read(owner("p")).await,
        Err(StoreError::Backend(_))
    ));
}

async fn aborted(s: &FileStorage) -> Result<()> {
    s.write(
        owner("p"),
        Lane::Entities,
        Box::new(|u| {
            u.create(task("lost"))?;
            Err(StoreError::InvalidPage("abort".into()))
        }),
    )
    .await
}

#[tokio::test]
async fn an_aborted_unit_of_work_writes_nothing() {
    let root = root();
    let s = FileStorage::new(root.path());
    assert!(aborted(&s).await.is_err());
    assert!(
        !root.path().read_dir().unwrap().any(|_| true),
        "a first write that aborts leaves no directory"
    );

    create(&s, "p", "kept").await;
    let [document] = documents(root.path()).try_into().unwrap();
    let before = std::fs::read(&document).unwrap();
    assert!(aborted(&s).await.is_err());
    assert_eq!(std::fs::read(&document).unwrap(), before);
}

#[tokio::test]
async fn a_read_only_unit_of_work_does_not_rewrite_the_document() {
    let root = root();
    let s = FileStorage::new(root.path());
    create(&s, "p", "a").await;
    let [document] = documents(root.path()).try_into().unwrap();
    // A commit replaces the file, so a new inode shows a rewrite.
    let inode =
        |path: &Path| std::os::unix::fs::MetadataExt::ino(&std::fs::metadata(path).unwrap());
    let before = inode(&document);
    s.write(
        owner("p"),
        Lane::Entities,
        Box::new(|u| {
            u.get(EntityKind::Task, "a")?;
            u.update(task("missing"), Revision::ONE).unwrap_err();
            Ok(())
        }),
    )
    .await
    .unwrap();
    assert_eq!(inode(&document), before);
}

#[tokio::test]
async fn a_project_id_never_names_a_path_outside_the_root() {
    let parent = root();
    let root = parent.path().join("store");
    std::fs::create_dir(&root).unwrap();
    let s = FileStorage::new(&root);
    for project in ["../escape", "a/b", "", "/abs", "..", "p-70"] {
        create(&s, project, "t").await;
    }
    // Six projects, six directories directly below the root, nothing beside it.
    assert_eq!(root.read_dir().unwrap().count(), 6);
    assert_eq!(parent.path().read_dir().unwrap().count(), 1);
    for project in ["../escape", "a/b", "", "/abs", "..", "p-70"] {
        assert!(get(&s, project, "t").await.is_some(), "{project:?}");
    }
    assert_eq!(get(&s, "other", "t").await, None);
}

#[tokio::test]
async fn a_caller_that_stops_waiting_does_not_open_the_gate_early() {
    let root = root();
    let s = std::sync::Arc::new(FileStorage::new(root.path()));
    let (entered, entered_rx) = std::sync::mpsc::channel();
    let (release, release_rx) = std::sync::mpsc::channel::<()>();
    let slow = {
        let s = s.clone();
        tokio::spawn(async move {
            s.write(
                owner("p"),
                Lane::Entities,
                Box::new(move |u| {
                    u.create(task("slow"))?;
                    entered.send(()).unwrap();
                    release_rx.recv().unwrap();
                    Ok(())
                }),
            )
            .await
        })
    };
    tokio::task::spawn_blocking(move || entered_rx.recv().unwrap())
        .await
        .unwrap();
    slow.abort();
    let _ = slow.await;
    let second = {
        let s = s.clone();
        tokio::spawn(async move {
            s.write(
                owner("p"),
                Lane::Entities,
                Box::new(|u| u.create(task("second")).map(|_| ())),
            )
            .await
        })
    };
    // The second writer cannot start while the first one's work still runs.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(!second.is_finished());
    release.send(()).unwrap();
    second.await.unwrap().unwrap();
    // Both committed, on top of each other.
    assert_eq!(get(&s, "p", "slow").await.unwrap().position, 1);
    assert_eq!(get(&s, "p", "second").await.unwrap().position, 2);
}

#[tokio::test]
async fn default_orders_put_their_own_column_before_the_id_tie_breaker() {
    // The shared scenarios give every row of these families one timestamp, so
    // only the ids and positions decide there; here the columns differ from both.
    let root = root();
    let s = FileStorage::new(root.path());
    let team = |id: &str, created: &str| {
        Entity::Team(Box::new(Team {
            id: id.into(),
            name: id.into(),
            description: None,
            status: TeamStatus::Active,
            members: vec![],
            task_pool: vec![],
            lead_agent_id: "agt_b".into(),
            created_at: created.into(),
            updated_at: created.into(),
            config: DEFAULT_TEAM_CONFIG,
        }))
    };
    let message = |id: &str, created: &str| {
        Entity::Message(Box::new(Message {
            id: id.into(),
            channel: MessageChannel::Direct,
            from_agent_id: "agt_a".into(),
            to_agent_id: None,
            subject: id.into(),
            body: String::new(),
            created_at: created.into(),
            expires_at: None,
            status: MessageStatus::Pending,
            delivered_at: None,
            team_id: None,
            reply_to: None,
        }))
    };
    let agent = |id: &str| {
        Entity::Agent(Box::new(orch_domain::Agent {
            id: id.into(),
            ..make_agent()
        }))
    };
    s.write(
        owner("p"),
        Lane::Entities,
        Box::new(move |u| {
            for value in [
                team("t-a-created-late", "2"),
                team("t-b-created-early", "1"),
                message("m-z", "1"),
                message("m-a", "1"),
                agent("agent-z"),
                agent("agent-a"),
            ] {
                u.create(value)?;
            }
            Ok(())
        }),
    )
    .await
    .unwrap();
    let snapshot = s.read(owner("p")).await.unwrap();
    for (filter, expected) in [
        (Filter::Teams, vec!["t-b-created-early", "t-a-created-late"]),
        (Filter::Messages { recipient: None }, vec!["m-z", "m-a"]),
        (Filter::Agents, vec!["agent-z", "agent-a"]),
    ] {
        let page = snapshot
            .list(&filter, &PageRequest::default())
            .await
            .unwrap();
        let ids: Vec<_> = page.items.iter().map(|r| r.entity.id()).collect();
        assert_eq!(ids, expected, "{filter:?}");
    }
}

fn event(seq: u64) -> SequencedEvent {
    SequencedEvent {
        seq,
        event: RunEvent {
            timestamp: "2026-01-01T00:00:00Z".into(),
            event_type: RunEventType::AgentOutput,
            data: serde_json::json!({ "seq": seq }),
        },
    }
}

fn new_lease(task: &str, agent: &str, run: &str) -> NewLease {
    NewLease {
        task: task.into(),
        agent: agent.into(),
        run: run.into(),
    }
}

async fn claim(s: &FileStorage, new: NewLease) -> Lease {
    s.transaction(owner("p"), Lane::Claims, move |u| {
        match u.claim_lease(&new, 1_000)? {
            ClaimOutcome::Granted(lease) => Ok(lease),
            ClaimOutcome::Held(lease) => panic!("held by {lease:?}"),
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn events_gaps_leases_and_epochs_survive_a_new_instance() {
    let root = root();
    let first = FileStorage::new(root.path());
    let one = claim(&first, new_lease("t", "a", "r1")).await;
    let (lineage, epoch) = (one.lineage.clone(), one.epoch);
    first
        .write(
            owner("p"),
            Lane::Events,
            Box::new(move |u| {
                u.append_events("r1", &lineage, epoch, &[event(2), event(1)])?;
                u.record_event_gap("r1", 2, 3)?;
                u.release_lease("t", &lineage, epoch)?;
                Ok(())
            }),
        )
        .await
        .unwrap();
    let second_lease = claim(&first, new_lease("t", "a", "r2")).await;
    drop(first);

    let second = FileStorage::new(root.path());
    let view = second.read(owner("p")).await.unwrap();
    assert_eq!(
        view.read_events("r1", 0, 10).await.unwrap(),
        vec![event(1), event(2)]
    );
    assert_eq!(
        view.read_event_gaps("r1").await.unwrap(),
        vec![EventGap {
            after_seq: 2,
            dropped: 3
        }]
    );
    assert_eq!(view.read_leases().await.unwrap(), vec![second_lease]);
    // The counter outlived the release and the instance.
    second
        .write(
            owner("p"),
            Lane::Claims,
            Box::new(|u| {
                let (lineage, epoch) = {
                    let lease = u.get_lease("t")?.unwrap();
                    (lease.lineage, lease.epoch)
                };
                u.release_lease("t", &lineage, epoch)?;
                Ok(())
            }),
        )
        .await
        .unwrap();
    assert_eq!(claim(&second, new_lease("t", "a", "r3")).await.epoch, 3);
}

#[tokio::test]
async fn a_restore_ends_leases_keeps_epochs_and_the_lineage_outlives_the_instance() {
    let root = root();
    let first = FileStorage::new(root.path());
    let before = claim(&first, new_lease("t", "a", "r1")).await;
    first.restore_to_lineage("after-restore").await.unwrap();
    drop(first);

    let second = FileStorage::new(root.path());
    assert!(
        second
            .read(owner("p"))
            .await
            .unwrap()
            .read_leases()
            .await
            .unwrap()
            .is_empty()
    );
    let after = claim(&second, new_lease("t", "a", "r2")).await;
    assert_eq!(after.lineage, "after-restore");
    assert_eq!((before.epoch, after.epoch), (1, 2));
}

#[tokio::test]
async fn a_lease_past_its_epoch_counter_is_refused() {
    let root = root();
    let s = FileStorage::new(root.path());
    claim(&s, new_lease("t", "a", "r1")).await;
    let [document] = documents(root.path()).try_into().unwrap();
    let mut value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&document).unwrap()).unwrap();
    value["leases"]["epochs"]["t"] = 0.into();
    std::fs::write(&document, serde_json::to_vec(&value).unwrap()).unwrap();
    assert!(matches!(
        s.read(owner("p")).await,
        Err(StoreError::Backend(_))
    ));
}

#[tokio::test]
async fn two_leases_on_one_agent_are_refused() {
    let root = root();
    let s = FileStorage::new(root.path());
    claim(&s, new_lease("t1", "a", "r1")).await;
    let [document] = documents(root.path()).try_into().unwrap();
    let mut value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&document).unwrap()).unwrap();
    let mut second = value["leases"]["leases"]["t1"].clone();
    second["run"] = "r2".into();
    value["leases"]["leases"]["t2"] = second;
    value["leases"]["epochs"]["t2"] = 1.into();
    std::fs::write(&document, serde_json::to_vec(&value).unwrap()).unwrap();
    assert!(matches!(
        s.read(owner("p")).await,
        Err(StoreError::Backend(_))
    ));
}

#[tokio::test]
async fn operations_that_change_nothing_do_not_rewrite_the_document() {
    let root = root();
    let s = FileStorage::new(root.path());
    let lease = claim(&s, new_lease("t", "a", "r1")).await;
    let (lineage, epoch) = (lease.lineage.clone(), lease.epoch);
    s.write(
        owner("p"),
        Lane::Events,
        Box::new({
            let lineage = lineage.clone();
            move |u| {
                u.append_events("r1", &lineage, epoch, &[event(1)])?;
                u.record_event_gap("r1", 1, 1)
            }
        }),
    )
    .await
    .unwrap();
    let [document] = documents(root.path()).try_into().unwrap();
    // A commit replaces the file, so a new inode shows a rewrite.
    let inode =
        |path: &Path| std::os::unix::fs::MetadataExt::ino(&std::fs::metadata(path).unwrap());
    let before = inode(&document);
    s.write(
        owner("p"),
        Lane::Events,
        Box::new(move |u| {
            // A duplicate batch, an empty batch, a repeated gap, a prune of
            // nothing, a refused claim and a refused renewal change no row.
            u.append_events("r1", &lineage, epoch, &[event(1)])?;
            u.append_events("r1", &lineage, epoch, &[])?;
            u.record_event_gap("r1", 1, 1)?;
            u.prune_events("other-run", 1)?;
            assert!(matches!(
                u.claim_lease(&new_lease("t", "b", "r9"), 0)?,
                ClaimOutcome::Held(_)
            ));
            u.renew_lease("t", &lineage, epoch + 1, 0).unwrap_err();
            Ok(())
        }),
    )
    .await
    .unwrap();
    assert_eq!(inode(&document), before);
}

fn stop_run(run: &str) -> IntentPayload {
    IntentPayload::ProcessStop { run: run.into() }
}

fn remove_workspace(task: &str) -> IntentPayload {
    IntentPayload::WorkspaceRemove { task: task.into() }
}

async fn add_intent(s: &FileStorage, payload: IntentPayload, now: i64) -> u64 {
    s.transaction(owner("p"), Lane::Entities, move |u| {
        Ok(u.add_intent(&payload, now)?.id)
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn intents_ids_and_completion_survive_a_new_instance() {
    let root = root();
    let first = FileStorage::new(root.path());
    claim(&first, new_lease("t", "a", "r1")).await;
    assert_eq!(add_intent(&first, stop_run("r1"), 10).await, 1);
    assert_eq!(add_intent(&first, remove_workspace("t"), 11).await, 2);
    first
        .transaction(owner("p"), Lane::Entities, |u| u.finish_intent(2, 12))
        .await
        .unwrap();
    drop(first);

    let second = FileStorage::new(root.path());
    let view = second.read(owner("p")).await.unwrap();
    let pending = view.read_pending_intents(0, 10).await.unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!((pending[0].id, pending[0].created_at), (1, 10));
    assert_eq!(pending[0].payload, stop_run("r1"));
    assert_eq!(
        view.read_intent(2).await.unwrap().unwrap().done_at,
        Some(12)
    );
    // Retention deletes the finished row; the counter outlives it and the instance.
    let deleted = second
        .transaction(owner("p"), Lane::Maintenance, |u| {
            u.delete_finished_intents(12 + DONE_RETENTION_MS, 10)
        })
        .await
        .unwrap();
    assert_eq!(deleted, 1);
    drop(view);
    drop(second);
    let third = FileStorage::new(root.path());
    assert_eq!(add_intent(&third, remove_workspace("t"), 13).await, 3);
    let finished = third
        .transaction(owner("p"), Lane::Entities, |u| u.finish_intent(2, 99))
        .await
        .unwrap();
    assert_eq!(finished, FinishOutcome::AlreadyDone);
}

#[tokio::test]
async fn a_document_from_before_intents_reads_as_an_empty_set() {
    let root = root();
    let s = FileStorage::new(root.path());
    create(&s, "p", "t1").await;
    let [document] = documents(root.path()).try_into().unwrap();
    let mut value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&document).unwrap()).unwrap();
    assert!(value.as_object_mut().unwrap().remove("intents").is_some());
    std::fs::write(&document, serde_json::to_vec(&value).unwrap()).unwrap();

    let reopened = FileStorage::new(root.path());
    let view = reopened.read(owner("p")).await.unwrap();
    assert!(view.read_pending_intents(0, 10).await.unwrap().is_empty());
    assert!(view.get(EntityKind::Task, "t1").await.unwrap().is_some());
    assert_eq!(add_intent(&reopened, remove_workspace("t1"), 1).await, 1);
}

#[tokio::test]
async fn intent_operations_that_change_nothing_do_not_rewrite_the_document() {
    let root = root();
    let s = FileStorage::new(root.path());
    claim(&s, new_lease("t", "a", "r1")).await;
    add_intent(&s, stop_run("r1"), 1).await;
    s.transaction(owner("p"), Lane::Entities, |u| u.finish_intent(1, 2))
        .await
        .unwrap();
    let [document] = documents(root.path()).try_into().unwrap();
    // A commit replaces the file, so a new inode shows a rewrite.
    let inode =
        |path: &Path| std::os::unix::fs::MetadataExt::ino(&std::fs::metadata(path).unwrap());
    let before = (inode(&document), std::fs::read(&document).unwrap());
    s.write(
        owner("p"),
        Lane::Entities,
        Box::new(|u| {
            // A repeated finish, a retention call with nothing due, reads, and
            // refusals that the callback catches change no row and spend no id.
            assert_eq!(u.finish_intent(1, 9)?, FinishOutcome::AlreadyDone);
            assert_eq!(u.delete_finished_intents(3, 10)?, 0);
            u.list_pending_intents(0, 10)?;
            u.get_intent(1)?;
            u.add_intent(&remove_workspace("../x"), 1).unwrap_err();
            u.add_intent(&stop_run("no-lease"), 1).unwrap_err();
            u.finish_intent(7, 1).unwrap_err();
            Ok(())
        }),
    )
    .await
    .unwrap();
    assert_eq!(
        (inode(&document), std::fs::read(&document).unwrap()),
        before
    );
}

#[tokio::test]
async fn a_failed_unit_of_work_keeps_no_intent_on_disk() {
    let root = root();
    let s = FileStorage::new(root.path());
    claim(&s, new_lease("t", "a", "r1")).await;
    let [document] = documents(root.path()).try_into().unwrap();
    let before = std::fs::read(&document).unwrap();
    let failed = s
        .transaction(owner("p"), Lane::Entities, |u| {
            u.add_intent(&stop_run("r1"), 1)?;
            Err::<(), _>(StoreError::CounterExhausted)
        })
        .await;
    assert!(matches!(failed, Err(StoreError::CounterExhausted)));
    assert_eq!(std::fs::read(&document).unwrap(), before);
    assert_eq!(add_intent(&s, stop_run("r1"), 2).await, 1);
}

async fn edited_intents(mutate: impl FnOnce(&mut serde_json::Value)) -> StoreError {
    let root = root();
    let s = FileStorage::new(root.path());
    claim(&s, new_lease("t", "a", "r1")).await;
    add_intent(&s, stop_run("r1"), 1).await;
    let [document] = documents(root.path()).try_into().unwrap();
    let mut value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&document).unwrap()).unwrap();
    mutate(&mut value["intents"]);
    std::fs::write(&document, serde_json::to_vec(&value).unwrap()).unwrap();
    FileStorage::new(root.path())
        .read(owner("p"))
        .await
        .err()
        .expect("a document that is not the contract is refused")
}

#[tokio::test]
async fn a_hand_edited_intent_outside_the_contract_is_refused_on_read() {
    // A path where an id belongs, an id past the counter, a kind nobody defined.
    for edit in [
        (|v: &mut serde_json::Value| v["rows"]["1"]["payload"]["run"] = "../escape".into())
            as fn(&mut serde_json::Value),
        |v| v["last"] = 0.into(),
        |v| v["rows"]["1"]["payload"]["kind"] = "process.kill".into(),
        |v| {
            v["rows"]["1"]["payload"]["pid"] = 42.into();
        },
    ] {
        assert!(matches!(edited_intents(edit).await, StoreError::Backend(_)));
    }
}

fn described(id: &str, text: String) -> Entity {
    Entity::Task(Box::new(Task {
        id: id.into(),
        description: text,
        ..make_task()
    }))
}

fn document(s: &FileStorage, project: &str) -> std::path::PathBuf {
    s.project_dir(project).join(super::DOCUMENT)
}

#[tokio::test]
async fn text_over_64_kib_round_trips_through_the_blob_store() {
    use crate::blob::{BlobHash, DiskBlobStore};
    use crate::data_dir::confined::ConfinedDir;
    let root = root();
    let s = FileStorage::new(root.path());
    let text = "é\n\"\\🦀".repeat(20_000);
    let value = described("large", text.clone());
    let expected = s
        .transaction(owner("p"), Lane::Entities, move |u| u.create(value))
        .await
        .unwrap();
    let bytes = std::fs::read(document(&s, "p")).unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        json["records"][0]["texts"][0]["sha256"],
        BlobHash::of(text.as_bytes()).to_string()
    );
    assert!(json["records"][0]["row"]["entity"]["value"]["description"].is_null());
    let dir = ConfinedDir::open(&s.project_dir("p")).unwrap();
    let blobs = DiskBlobStore::open(&dir).unwrap();
    assert_eq!(
        blobs.get(&BlobHash::of(text.as_bytes())).unwrap(),
        text.as_bytes()
    );
    drop(s);
    let reopened = FileStorage::new(root.path());
    assert_eq!(get(&reopened, "p", "large").await, Some(expected.clone()));
    let page = reopened
        .read(owner("p"))
        .await
        .unwrap()
        .list(
            &Filter::Tasks {
                status: None,
                goal: None,
            },
            &PageRequest::default(),
        )
        .await
        .unwrap();
    assert_eq!(page.items, vec![expected]);
}

#[tokio::test]
async fn inline_threshold_counts_utf8_bytes_and_preserves_empty_values() {
    use crate::blob::INLINE_TEXT_MAX_BYTES as MAX;
    let root = root();
    let s = FileStorage::new(root.path());
    for (index, text) in [
        String::new(),
        "x".repeat(MAX - 1),
        "é".repeat(MAX / 2),
        "x".repeat(MAX + 1),
        "é".repeat(MAX / 2 + 1),
    ]
    .into_iter()
    .enumerate()
    {
        let project = index.to_string();
        let external = text.len() > MAX;
        let entity = described("t", text);
        let expected = s
            .transaction(owner(&project), Lane::Entities, move |u| u.create(entity))
            .await
            .unwrap();
        let bytes = std::fs::read(document(&s, &project)).unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["records"][0].get("texts").is_some(), external);
        // Inline rows retain the staged reader's real Record encoding.
        if !external {
            assert_eq!(
                serde_json::from_value::<Record>(json["records"][0].clone()).unwrap(),
                expected
            );
        }
        assert_eq!(get(&s, &project, "t").await, Some(expected));
    }
}

#[tokio::test]
async fn large_text_is_isolated_by_project_and_updates_keep_old_snapshots() {
    let root = root();
    let s = FileStorage::new(root.path());
    for project in ["../p", "other"] {
        let value = described("same", project.repeat(70_000));
        s.transaction(owner(project), Lane::Entities, move |u| u.create(value))
            .await
            .unwrap();
    }
    let old = s.read(owner("../p")).await.unwrap();
    let before = old.get(EntityKind::Task, "same").await.unwrap();
    let other = get(&s, "other", "same").await;
    assert_ne!(before, other);
    assert_eq!(root.path().read_dir().unwrap().count(), 2);
    let replacement = described("same", "small".into());
    s.transaction(owner("../p"), Lane::Entities, move |u| {
        u.update(replacement, Revision::ONE)
    })
    .await
    .unwrap();
    assert_eq!(old.get(EntityKind::Task, "same").await.unwrap(), before);
    assert_eq!(get(&s, "other", "same").await, other);
    assert_eq!(get(&s, "absent", "same").await, None);
    assert_eq!(get(&s, "../p", "same").await.unwrap().revision.get(), 2);
}

#[tokio::test]
async fn returned_callback_errors_roll_back_large_text_rows_and_counters() {
    let root = root();
    let s = FileStorage::new(root.path());
    create(&s, "p", "kept").await;
    let path = document(&s, "p");
    let before = std::fs::read(&path).unwrap();
    let result = s
        .write(
            owner("p"),
            Lane::Entities,
            Box::new(|u| {
                u.update(described("kept", "changed".repeat(70_000)), Revision::ONE)?;
                u.create(described("lost", "large".repeat(70_000)))?;
                Err(StoreError::InvalidPage("abort".into()))
            }),
        )
        .await;
    assert!(result.is_err());
    assert_eq!(std::fs::read(path).unwrap(), before);
    assert!(!s.project_dir("p").join("blobs").exists());
    assert_eq!(create(&s, "p", "next").await.position, 2);
    assert_eq!(get(&s, "p", "lost").await, None);
}

#[tokio::test]
async fn returned_blob_errors_preserve_the_committed_document() {
    let root = root();
    let s = FileStorage::new(root.path());
    create(&s, "p", "kept").await;
    let path = document(&s, "p");
    let before = std::fs::read(&path).unwrap();
    // A regular file cannot serve as the confined blob directory.
    std::fs::write(s.project_dir("p").join("blobs"), b"blocked").unwrap();
    let result = s
        .transaction(owner("p"), Lane::Entities, |u| {
            u.update(described("kept", "x".repeat(65_537)), Revision::ONE)
        })
        .await;
    assert!(matches!(result, Err(StoreError::Backend(_))));
    assert_eq!(std::fs::read(path).unwrap(), before);
    assert_eq!(get(&s, "p", "kept").await.unwrap().revision, Revision::ONE);
}

#[tokio::test]
async fn missing_corrupt_or_invalid_hashes_fail_without_rewriting_rows() {
    use crate::blob::BlobHash;
    let root = root();
    for mode in ["missing", "corrupt", "invalid", "duplicate"] {
        let s = FileStorage::new(root.path().join(mode));
        let text = "x".repeat(65_537);
        let hash = BlobHash::of(text.as_bytes()).to_string();
        s.transaction(owner("p"), Lane::Entities, move |u| {
            u.create(described("t", text))
        })
        .await
        .unwrap();
        let path = document(&s, "p");
        let blob = s
            .project_dir("p")
            .join("blobs/sha256")
            .join(&hash[..2])
            .join(&hash[2..4])
            .join(&hash);
        match mode {
            "missing" => std::fs::remove_file(blob).unwrap(),
            "corrupt" => std::fs::write(blob, b"bad").unwrap(),
            _ => {
                let mut json: serde_json::Value =
                    serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
                if mode == "invalid" {
                    json["records"][0]["texts"][0]["sha256"] = "../outside".into();
                } else {
                    let item = json["records"][0]["texts"][0].clone();
                    json["records"][0]["texts"]
                        .as_array_mut()
                        .unwrap()
                        .push(item);
                }
                std::fs::write(&path, serde_json::to_vec(&json).unwrap()).unwrap();
            }
        }
        let before = std::fs::read(&path).unwrap();
        assert!(
            matches!(s.read(owner("p")).await, Err(StoreError::Backend(_))),
            "{mode}"
        );
        assert!(
            s.transaction(owner("p"), Lane::Entities, |u| u.create(task("new")))
                .await
                .is_err(),
            "{mode}"
        );
        assert_eq!(std::fs::read(path).unwrap(), before);
    }
}

#[tokio::test]
async fn message_context_and_nested_optional_text_round_trip_together() {
    let root = root();
    let s = FileStorage::new(root.path());
    let large = "synthetic\0é\n".repeat(10_000);
    let mut t = make_task();
    t.id = "nested".into();
    t.proof = Some(orch_domain::TaskProof {
        files_changed: vec![large.clone()],
        test_results: Some(large.clone()),
        agent_summary: Some(String::new()),
        ..Default::default()
    });
    let entities = vec![
        Entity::Task(Box::new(t)),
        Entity::Context(orch_core::storage::ContextEntry {
            key: "context/~key".into(),
            value: large.clone(),
            created_at: "1".into(),
            updated_at: "1".into(),
            ttl_ms: None,
            expires_at: None,
        }),
        Entity::Message(Box::new(Message {
            id: "message".into(),
            channel: MessageChannel::Direct,
            from_agent_id: "synthetic".into(),
            to_agent_id: None,
            subject: String::new(),
            body: large,
            created_at: "1".into(),
            expires_at: None,
            status: MessageStatus::Pending,
            delivered_at: None,
            team_id: None,
            reply_to: None,
        })),
    ];
    let expected = s
        .transaction(owner("p"), Lane::Entities, move |u| {
            entities
                .into_iter()
                .map(|e| u.create(e))
                .collect::<Result<Vec<_>>>()
        })
        .await
        .unwrap();
    drop(s);
    let s = FileStorage::new(root.path());
    let read = s.read(owner("p")).await.unwrap();
    for row in expected {
        assert_eq!(
            read.get(row.entity.kind(), row.entity.id()).await.unwrap(),
            Some(row.clone())
        );
    }
    let json: serde_json::Value =
        serde_json::from_slice(&std::fs::read(document(&s, "p")).unwrap()).unwrap();
    assert_eq!(json["records"].as_array().unwrap().len(), 3);
    // One content hash per project even across fields and families.
    let hashes: std::collections::BTreeSet<_> = json["records"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|row| row["texts"].as_array().unwrap())
        .map(|text| text["sha256"].as_str().unwrap())
        .collect();
    assert_eq!(hashes.len(), 1);
}

#[tokio::test]
async fn partial_blob_publication_leaves_no_rows_and_keeps_reusable_bytes() {
    use crate::blob::{BlobHash, DiskBlobStore};
    use crate::data_dir::confined::ConfinedDir;
    let root = root();
    let s = FileStorage::new(root.path());
    create(&s, "p", "kept").await;
    let path = document(&s, "p");
    let before = std::fs::read(&path).unwrap();
    let first = "a".repeat(65_537);
    let second = "z".repeat(65_537);
    let first_hash = BlobHash::of(first.as_bytes());
    let second_hash = BlobHash::of(second.as_bytes()).to_string();
    assert_ne!(&first_hash.to_string()[..2], &second_hash[..2]);
    let confined = ConfinedDir::open(&s.project_dir("p")).unwrap();
    let blobs = DiskBlobStore::open(&confined).unwrap();
    // Block only the second hash's shard, after the first blob can be published.
    std::fs::write(
        s.project_dir("p")
            .join("blobs/sha256")
            .join(&second_hash[..2]),
        b"blocked",
    )
    .unwrap();
    let result = s
        .write(
            owner("p"),
            Lane::Entities,
            Box::new(move |u| {
                u.create(described("a", first))?;
                u.create(described("b", second))?;
                Ok(())
            }),
        )
        .await;
    assert!(matches!(result, Err(StoreError::Backend(_))));
    assert_eq!(std::fs::read(path).unwrap(), before);
    assert_eq!(
        blobs.get(&first_hash).unwrap(),
        "a".repeat(65_537).as_bytes()
    );
    assert_eq!(get(&s, "p", "a").await, None);
    assert_eq!(get(&s, "p", "b").await, None);
    // Retrying the intact blob is safe; counters from the failed commit were not consumed.
    let row = s
        .transaction(owner("p"), Lane::Entities, |u| {
            u.create(described("a", "a".repeat(65_537)))
        })
        .await
        .unwrap();
    assert_eq!((row.revision, row.position), (Revision::ONE, 2));
}

#[tokio::test]
async fn large_text_commits_and_rollbacks_preserve_events_and_leases() {
    let root = root();
    let s = FileStorage::new(root.path());
    let lease = claim(&s, new_lease("t", "a", "r")).await;
    let expected = s
        .transaction(owner("p"), Lane::Entities, move |u| {
            u.append_events("r", &lease.lineage, lease.epoch, &[event(1)])?;
            u.record_event_gap("r", 1, 2)?;
            u.create(described("t", "x".repeat(65_537)))
        })
        .await
        .unwrap();
    let before = std::fs::read(document(&s, "p")).unwrap();
    let lease = s
        .read(owner("p"))
        .await
        .unwrap()
        .read_leases()
        .await
        .unwrap()
        .remove(0);
    let result = s
        .write(
            owner("p"),
            Lane::Entities,
            Box::new(move |u| {
                u.append_events("r", &lease.lineage, lease.epoch, &[event(2)])?;
                u.release_lease("t", &lease.lineage, lease.epoch)?;
                u.update(described("t", "y".repeat(65_537)), Revision::ONE)?;
                Err(StoreError::InvalidPage("abort".into()))
            }),
        )
        .await;
    assert!(result.is_err());
    assert_eq!(std::fs::read(document(&s, "p")).unwrap(), before);
    drop(s);
    let s = FileStorage::new(root.path());
    let view = s.read(owner("p")).await.unwrap();
    assert_eq!(
        view.get(EntityKind::Task, "t").await.unwrap(),
        Some(expected.clone())
    );
    assert_eq!(view.read_events("r", 0, 10).await.unwrap(), vec![event(1)]);
    assert_eq!(
        view.read_event_gaps("r").await.unwrap(),
        vec![EventGap {
            after_seq: 1,
            dropped: 2
        }]
    );
    assert_eq!(view.read_leases().await.unwrap().len(), 1);
    s.restore_to_lineage("restored").await.unwrap();
    assert_eq!(get(&s, "p", "t").await, Some(expected));
    let restored = s.read(owner("p")).await.unwrap();
    assert!(restored.read_leases().await.unwrap().is_empty());
    assert_eq!(
        restored.read_events("r", 0, 10).await.unwrap(),
        vec![event(1)]
    );
    assert_eq!(claim(&s, new_lease("t", "a", "next")).await.epoch, 2);
}
