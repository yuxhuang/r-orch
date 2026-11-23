//! What only a file reference can get wrong: the document on disk. The shared
//! scenarios in `orch-engine`'s `storage_conformance` cover the port's rules.

use std::path::Path;

use orch_core::store::events::{EventGap, SequencedEvent};
use orch_core::store::intents::{DONE_RETENTION_MS, FinishOutcome, IntentPayload};
use orch_core::store::leases::{ClaimOutcome, Lease, NewLease};
use orch_core::store::request_keys::{KEY_TTL_MS, KeyCheck, RequestScope};
use orch_core::store::retries::Retry;
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

enum Corruption {
    Parse(&'static str),
    Invalid(&'static str),
}

fn assert_corruption(error: StoreError, path: &Path, expected: Corruption) {
    match expected {
        Corruption::Invalid(detail) => assert_document_error(error, path, detail),
        Corruption::Parse(detail) => match error {
            StoreError::Backend(orch_core::Error::Serde {
                context,
                path: reported,
                source,
            }) => {
                assert_eq!(context, "cannot parse the storage document");
                assert_eq!(reported.as_deref(), Some(path));
                assert!(source.to_string().contains(detail), "{source}");
            }
            other => panic!("expected a parse error, got {other:?}"),
        },
    }
}

fn assert_parse_error(error: StoreError, document: &Path) {
    match error {
        StoreError::Backend(orch_core::Error::Serde {
            context,
            path,
            source,
        }) => {
            assert_eq!(context, "cannot parse the storage document");
            assert_eq!(path.as_deref(), Some(document));
            assert!(source.to_string().contains("key must be a string"));
        }
        other => panic!("expected a parse error, got {other:?}"),
    }
}

fn assert_backend_message(error: StoreError, expected: &str) {
    match error {
        StoreError::Backend(orch_core::Error::Domain(orch_domain::DomainError::Other(message))) => {
            assert_eq!(message, expected);
        }
        other => panic!("expected a diagnostic for corrupt data, got {other:?}"),
    }
}

fn assert_document_error(error: StoreError, document: &Path, detail: &str) {
    assert_backend_message(
        error,
        &format!(
            "{} is not a valid storage document: {detail}",
            document.display()
        ),
    );
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

    assert_parse_error(s.read(owner("p")).await.err().unwrap(), &document);
    let write = s
        .write(
            owner("p"),
            Lane::Entities,
            Box::new(|u| u.create(task("b")).map(|_| ())),
        )
        .await;
    assert_parse_error(write.unwrap_err(), &document);
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
    assert_document_error(
        s.read(owner("p")).await.err().unwrap(),
        &document,
        "Task a appears twice",
    );
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
    assert_document_error(
        s.read(owner("p")).await.err().unwrap(),
        &document,
        "Task a is past its counter",
    );
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
    assert_document_error(
        s.read(owner("p")).await.err().unwrap(),
        &document,
        "the lease of t is past its epoch counter",
    );
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
    assert_document_error(
        s.read(owner("p")).await.err().unwrap(),
        &document,
        "the lease of t2 shares an agent or a run",
    );
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

async fn edited_intents(
    mutate: impl FnOnce(&mut serde_json::Value),
) -> (std::path::PathBuf, StoreError) {
    let root = root();
    let s = FileStorage::new(root.path());
    claim(&s, new_lease("t", "a", "r1")).await;
    add_intent(&s, stop_run("r1"), 1).await;
    let [document] = documents(root.path()).try_into().unwrap();
    let mut value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&document).unwrap()).unwrap();
    mutate(&mut value["intents"]);
    std::fs::write(&document, serde_json::to_vec(&value).unwrap()).unwrap();
    let error = FileStorage::new(root.path())
        .read(owner("p"))
        .await
        .err()
        .expect("a document that is not the contract is refused");
    (document, error)
}

#[tokio::test]
async fn a_hand_edited_intent_outside_the_contract_is_refused_on_read() {
    // A path where an id belongs, an id past the counter, a kind nobody defined.
    let edits = [
        (|v: &mut serde_json::Value| v["rows"]["1"]["payload"]["run"] = "../escape".into())
            as fn(&mut serde_json::Value),
        |v| v["last"] = 0.into(),
        |v| v["rows"]["1"]["payload"]["kind"] = "process.kill".into(),
        |v| {
            v["rows"]["1"]["payload"]["pid"] = 42.into();
        },
    ];
    let expected = [
        Corruption::Invalid(
            "intent 1: invalid intent: an intent names one id of 1 to 128 bytes of A-Z a-z 0-9 . _ -, other than . and ..",
        ),
        Corruption::Invalid("intent 1 is past its counter"),
        Corruption::Parse("unknown variant `process.kill`"),
        Corruption::Parse("unknown field `pid`"),
    ];
    assert_eq!(edits.len(), expected.len());
    for (edit, expected) in edits.into_iter().zip(expected) {
        let (path, error) = edited_intents(edit).await;
        assert_corruption(error, &path, expected);
    }
}

const REQ: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn key_scope(n: &str) -> RequestScope {
    RequestScope::of(&owner("p"), &format!("{n:_<16}")).unwrap()
}

async fn store_key(s: &FileStorage, n: &str, result: &str, now: i64) {
    let scope = key_scope(n);
    let result = result.to_string();
    s.transaction(owner("p"), Lane::Entities, move |u| {
        u.store_request_key(&scope, "task.update", REQ, &result, now)
    })
    .await
    .unwrap();
}

async fn check_key(s: &FileStorage, n: &str, now: i64) -> KeyCheck {
    let scope = key_scope(n);
    s.transaction(owner("p"), Lane::Entities, move |u| {
        u.check_request_key(&scope, "task.update", REQ, now)
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn request_keys_and_their_results_survive_a_new_instance() {
    let root = root();
    let first = FileStorage::new(root.path());
    store_key(&first, "kept", "first", 1_000).await;
    store_key(&first, "big", &"x".repeat(65_537), 1_000).await;
    drop(first);

    let second = FileStorage::new(root.path());
    assert_eq!(
        check_key(&second, "kept", 1_001).await,
        KeyCheck::Replay("first".into())
    );
    assert_eq!(
        check_key(&second, "big", 1_001).await,
        KeyCheck::ReplayWithoutResult
    );
    // The expiry is the stored time plus the hour, not a time of this instance.
    assert_eq!(
        check_key(&second, "kept", 1_000 + KEY_TTL_MS - 1).await,
        KeyCheck::Replay("first".into())
    );
    assert_eq!(
        check_key(&second, "kept", 1_000 + KEY_TTL_MS).await,
        KeyCheck::New
    );
    let deleted = second
        .transaction(owner("p"), Lane::Maintenance, |u| {
            u.delete_expired_request_keys(1_000 + KEY_TTL_MS, 10)
        })
        .await
        .unwrap();
    assert_eq!(deleted, 2);
    drop(second);
    let third = FileStorage::new(root.path());
    assert_eq!(check_key(&third, "kept", 1_001).await, KeyCheck::New);
}

#[tokio::test]
async fn a_document_from_before_request_keys_reads_as_an_empty_set() {
    let root = root();
    let s = FileStorage::new(root.path());
    create(&s, "p", "t1").await;
    let [document] = documents(root.path()).try_into().unwrap();
    let mut value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&document).unwrap()).unwrap();
    assert!(
        value
            .as_object_mut()
            .unwrap()
            .remove("request_keys")
            .is_some()
    );
    std::fs::write(&document, serde_json::to_vec(&value).unwrap()).unwrap();

    let reopened = FileStorage::new(root.path());
    assert_eq!(check_key(&reopened, "any", 1).await, KeyCheck::New);
    assert!(get(&reopened, "p", "t1").await.is_some());
    store_key(&reopened, "any", "r", 1).await;
    assert_eq!(
        check_key(&reopened, "any", 2).await,
        KeyCheck::Replay("r".into())
    );
}

#[tokio::test]
async fn request_key_operations_that_change_nothing_do_not_rewrite_the_document() {
    let root = root();
    let s = FileStorage::new(root.path());
    store_key(&s, "live", "first", 1_000).await;
    let [document] = documents(root.path()).try_into().unwrap();
    // A commit replaces the file, so a new inode shows a rewrite.
    let inode =
        |path: &Path| std::os::unix::fs::MetadataExt::ino(&std::fs::metadata(path).unwrap());
    let before = (inode(&document), std::fs::read(&document).unwrap());
    let live = key_scope("live");
    s.write(
        owner("p"),
        Lane::Entities,
        Box::new(move |u| {
            // Lookups, an expiry call with nothing due, and refusals that the
            // callback catches change no row.
            assert_eq!(
                u.check_request_key(&live, "task.update", REQ, 2_000)?,
                KeyCheck::Replay("first".into())
            );
            assert_eq!(u.delete_expired_request_keys(2_000, 10)?, 0);
            u.store_request_key(&live, "task.update", REQ, "again", 2_000)
                .unwrap_err();
            u.store_request_key(&live, "bad op", REQ, "x", 2_000)
                .unwrap_err();
            u.delete_expired_request_keys(2_000, 0).unwrap_err();
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
async fn a_failed_unit_of_work_keeps_no_request_key_on_disk() {
    let root = root();
    let s = FileStorage::new(root.path());
    create(&s, "p", "t1").await;
    let [document] = documents(root.path()).try_into().unwrap();
    let before = std::fs::read(&document).unwrap();
    let scope = key_scope("failed");
    let failed = s
        .transaction(owner("p"), Lane::Entities, move |u| {
            u.store_request_key(&scope, "task.update", REQ, "lost", 1)?;
            Err::<(), _>(StoreError::CounterExhausted)
        })
        .await;
    assert!(matches!(failed, Err(StoreError::CounterExhausted)));
    assert_eq!(std::fs::read(&document).unwrap(), before);
    assert_eq!(check_key(&s, "failed", 2).await, KeyCheck::New);
}

async fn edited_request_keys(
    mutate: impl FnOnce(&mut serde_json::Value),
) -> (std::path::PathBuf, StoreError) {
    let root = root();
    let s = FileStorage::new(root.path());
    store_key(&s, "first", "r", 1).await;
    store_key(&s, "second", "r", 1).await;
    let [document] = documents(root.path()).try_into().unwrap();
    let mut value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&document).unwrap()).unwrap();
    mutate(&mut value["request_keys"]);
    std::fs::write(&document, serde_json::to_vec(&value).unwrap()).unwrap();
    let error = FileStorage::new(root.path())
        .read(owner("p"))
        .await
        .err()
        .expect("a document that is not the contract is refused");
    (document, error)
}

#[tokio::test]
async fn a_hand_edited_request_key_outside_the_contract_is_refused_on_read() {
    let edits = [
        // A key outside the shape, a hash that is not a hash, an operation with a path in it.
        (|v: &mut serde_json::Value| v["rows"][0]["key"] = "short".into())
            as fn(&mut serde_json::Value),
        |v| v["rows"][0]["req_sha256"] = "ABC".into(),
        |v| v["rows"][0]["op"] = "../escape".into(),
        // A result over the bound, an expiry that is not an hour on, a repeated row.
        |v| v["rows"][0]["result"] = "x".repeat(65_537).into(),
        |v| v["rows"][0]["expires_at"] = 5.into(),
        |v| v["rows"][1] = v["rows"][0].clone(),
        |v| v["rows"].as_array_mut().unwrap().reverse(),
        // A run without an epoch, and a field nobody defined.
        |v| v["rows"][0]["run"] = "r1".into(),
        |v| v["rows"][0]["pid"] = 42.into(),
        |v| {
            v["rows"][0]["run"] = "r1".into();
            v["rows"][0]["epoch"] = (i64::MAX as u64 + 1).into();
        },
    ];
    let expected = [
        Corruption::Invalid(
            "request key 0: invalid request key: a key is 16 to 128 characters of A-Z a-z 0-9 _ -",
        ),
        Corruption::Invalid(
            "request key 0: invalid request key: a request hash is 64 lowercase hex digits",
        ),
        Corruption::Invalid(
            "request key 0: invalid request key: an operation is 1 to 128 bytes of A-Z a-z 0-9 . _ -",
        ),
        Corruption::Invalid("request key 0 keeps a result over the bound"),
        Corruption::Invalid("request key 0 does not expire an hour after it was stored"),
        Corruption::Invalid("request key 1 is out of order or repeated"),
        Corruption::Invalid("request key 1 is out of order or repeated"),
        Corruption::Invalid("request key 0 has an impossible scope"),
        Corruption::Parse("unknown field `pid`"),
        Corruption::Invalid("request key 0 has an impossible scope"),
    ];
    assert_eq!(edits.len(), expected.len());
    for (edit, expected) in edits.into_iter().zip(expected) {
        let (path, error) = edited_request_keys(edit).await;
        assert_corruption(error, &path, expected);
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
    assert_backend_message(
        result.unwrap_err(),
        "file reference text: not a regular file or expected directory: \"blobs\"",
    );
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
        let detail = match mode {
            "missing" => format!("no blob {hash}"),
            "corrupt" => format!("blob {hash} does not hash to its name"),
            "invalid" => "not a blob hash: 10 bytes, not 64".into(),
            "duplicate" => "text slot is not null or appears twice".into(),
            _ => unreachable!(),
        };
        let expected = format!("file reference text: {detail}");
        assert_backend_message(s.read(owner("p")).await.err().unwrap(), &expected);
        assert_backend_message(
            s.transaction(owner("p"), Lane::Entities, |u| u.create(task("new")))
                .await
                .unwrap_err(),
            &expected,
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
    assert_backend_message(
        result.unwrap_err(),
        &format!(
            "file reference text: not a regular file or expected directory: {:?}",
            &second_hash[..2]
        ),
    );
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

fn retry(task: &str, attempt: u32, due_at: i64) -> Retry {
    Retry {
        task: task.into(),
        attempt,
        due_at,
        error: "boom".into(),
    }
}

async fn due(s: &FileStorage, project: &str) -> Vec<Retry> {
    s.read(owner(project))
        .await
        .unwrap()
        .read_due_retries(i64::MAX, 1_000)
        .await
        .unwrap()
}

#[tokio::test]
async fn retry_rows_survive_a_new_instance_and_a_replacement_is_kept() {
    let root = root();
    let first = FileStorage::new(root.path());
    first
        .transaction(owner("p"), Lane::Entities, |u| {
            u.put_retry(&retry("t1", 1, 20))?;
            u.put_retry(&retry("t2", 2, 10))?;
            u.put_retry(&retry("t1", 3, 30))
        })
        .await
        .unwrap();
    drop(first);
    let second = FileStorage::new(root.path());
    assert_eq!(
        due(&second, "p").await,
        vec![retry("t2", 2, 10), retry("t1", 3, 30)]
    );
    assert!(due(&second, "other").await.is_empty());
    second
        .transaction(owner("p"), Lane::Entities, |u| u.delete_retry("t2"))
        .await
        .unwrap();
    drop(second);
    assert_eq!(
        due(&FileStorage::new(root.path()), "p").await,
        vec![retry("t1", 3, 30)]
    );
}

#[tokio::test]
async fn a_document_written_before_retries_existed_loads_with_none() {
    let root = root();
    let s = FileStorage::new(root.path());
    create(&s, "p", "t1").await;
    let [document] = documents(root.path()).try_into().unwrap();
    let mut value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&document).unwrap()).unwrap();
    assert!(value.as_object_mut().unwrap().remove("retries").is_some());
    std::fs::write(&document, serde_json::to_vec(&value).unwrap()).unwrap();
    assert!(due(&s, "p").await.is_empty());
    assert!(get(&s, "p", "t1").await.is_some());
}

#[tokio::test]
async fn retry_operations_that_change_nothing_do_not_rewrite_the_document() {
    let root = root();
    let s = FileStorage::new(root.path());
    s.transaction(owner("p"), Lane::Entities, |u| {
        u.put_retry(&retry("t1", 1, 5))
    })
    .await
    .unwrap();
    let [document] = documents(root.path()).try_into().unwrap();
    let inode =
        |path: &Path| std::os::unix::fs::MetadataExt::ino(&std::fs::metadata(path).unwrap());
    let before = inode(&document);
    s.transaction(owner("p"), Lane::Entities, |u| {
        // A delete of a missing row, a refused row and the reads change nothing.
        assert!(!u.delete_retry("never")?);
        u.put_retry(&retry("t1", 0, 5)).unwrap_err();
        assert_eq!(u.due_retries(i64::MAX, 10)?.len(), 1);
        Ok(())
    })
    .await
    .unwrap();
    assert_eq!(inode(&document), before);
}

#[tokio::test]
async fn attention_load_checks_the_session_scope_before_any_write() {
    use orch_core::store::attention::{AttentionFacts, DiskSubject};
    let root = root();
    let storage = FileStorage::new(root.path());
    storage
        .transaction(owner(""), Lane::Maintenance, |u| {
            u.open_attention(
                &AttentionFacts::DiskFloor {
                    subject: DiskSubject::DataDirectory,
                    free_bytes: 1,
                    floor_bytes: 100,
                },
                20,
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let daemon = storage.project_dir("").join("state.json");
    let project = storage.project_dir("p");
    std::fs::create_dir_all(&project).unwrap();
    let document = project.join("state.json");
    let bytes = std::fs::read(&daemon).unwrap();
    std::fs::write(&document, &bytes).unwrap();
    assert!(matches!(
        storage.read(owner("p")).await,
        Err(StoreError::InvalidAttention(_))
    ));
    assert!(matches!(
        storage
            .transaction(owner("p"), Lane::Entities, |u| {
                u.create(task("must_not_commit"))?;
                Ok(())
            })
            .await,
        Err(StoreError::InvalidAttention(_))
    ));
    assert_eq!(std::fs::read(&document).unwrap(), bytes);
    assert!(
        storage
            .read(owner(""))
            .await
            .unwrap()
            .read_attention(1)
            .await
            .unwrap()
            .is_some()
    );
}

// Issue #130 / PR142: exercise the persisted reader, including both read and
// write entry points, so a rejected document cannot be silently replaced.
async fn rejected_external_row(
    edit: impl FnOnce(&mut serde_json::Value, &crate::blob::DiskBlobStore),
    detail: &str,
) {
    use crate::blob::DiskBlobStore;
    use crate::data_dir::confined::ConfinedDir;
    let root = root();
    let s = FileStorage::new(root.path());
    s.transaction(owner("p"), Lane::Entities, |u| {
        u.create(described("t", "x".repeat(65_537)))
    })
    .await
    .unwrap();
    let path = document(&s, "p");
    let mut json: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let dir = ConfinedDir::open(&s.project_dir("p")).unwrap();
    let blobs = DiskBlobStore::open(&dir).unwrap();
    edit(&mut json["records"][0], &blobs);
    let before = serde_json::to_vec(&json).unwrap();
    std::fs::write(&path, &before).unwrap();
    drop(s);
    let reopened = FileStorage::new(root.path());
    let expected = format!("file reference text: {detail}");
    assert_backend_message(reopened.read(owner("p")).await.err().unwrap(), &expected);
    assert_backend_message(
        reopened
            .transaction(owner("p"), Lane::Entities, |u| u.create(task("new")))
            .await
            .unwrap_err(),
        &expected,
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

#[tokio::test]
async fn external_row_without_references_is_refused_without_rewriting() {
    rejected_external_row(
        |row, _| row["texts"] = serde_json::json!([]),
        "external row has no text references",
    )
    .await;
}

#[tokio::test]
async fn external_row_with_an_absent_pointer_is_refused_without_rewriting() {
    rejected_external_row(
        |row, _| row["texts"][0]["pointer"] = "/entity/value/absent".into(),
        "missing text slot",
    )
    .await;
}

#[tokio::test]
async fn external_row_with_an_invalid_pointer_is_refused_without_rewriting() {
    rejected_external_row(
        |row, _| row["texts"][0]["pointer"] = "entity/value/description".into(),
        "missing text slot",
    )
    .await;
}

#[tokio::test]
async fn external_row_with_a_non_null_target_is_refused_without_rewriting() {
    rejected_external_row(
        |row, _| row["row"]["entity"]["value"]["description"] = "inline".into(),
        "text slot is not null or appears twice",
    )
    .await;
}

#[tokio::test]
async fn external_row_with_correctly_hashed_non_utf8_bytes_is_refused_without_rewriting() {
    rejected_external_row(
        |row, blobs| {
            row["texts"][0]["sha256"] = blobs.put(&[0xff]).unwrap().to_string().into();
        },
        "invalid utf-8 sequence of 1 bytes from index 0",
    )
    .await;
}

#[tokio::test]
async fn external_row_with_invalid_hydrated_record_is_refused_without_rewriting() {
    rejected_external_row(
        |row, _| row["row"]["position"] = "invalid".into(),
        "invalid type: string \"invalid\", expected u64",
    )
    .await;
}

#[tokio::test]
async fn escaped_object_keys_and_nonzero_array_indexes_reopen_with_exact_values() {
    let root = root();
    let s = FileStorage::new(root.path());
    let mut agent = make_agent();
    agent.id = "agent".into();
    for (index, key) in ["z/first~", "~1", "a/b"].into_iter().enumerate() {
        agent
            .config
            .env
            .get_or_insert_with(Default::default)
            .insert(key.into(), format!("{index}é").repeat(30_000));
    }
    let mut task = make_task();
    task.id = "task".into();
    task.proof = Some(orch_domain::TaskProof {
        files_changed: vec![
            "zero".into(),
            "one".repeat(30_000),
            "two".into(),
            "three".repeat(20_000),
        ],
        ..Default::default()
    });
    let expected = s
        .transaction(owner("p"), Lane::Entities, move |u| {
            Ok(vec![
                u.create(Entity::Agent(Box::new(agent)))?,
                u.create(Entity::Task(Box::new(task)))?,
            ])
        })
        .await
        .unwrap();
    drop(s);
    let reopened = FileStorage::new(root.path());
    let read = reopened.read(owner("p")).await.unwrap();
    let agent = read.get(EntityKind::Agent, "agent").await.unwrap().unwrap();
    let Entity::Agent(agent) = agent.entity else {
        panic!("expected agent")
    };
    assert_eq!(
        agent
            .config
            .env
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        vec!["z/first~", "~1", "a/b"]
    );
    for record in expected {
        assert_eq!(
            read.get(record.entity.kind(), record.entity.id())
                .await
                .unwrap(),
            Some(record)
        );
    }
}

#[tokio::test]
async fn malformed_inline_record_preserves_parse_error_path_and_variant_diagnostic() {
    let root = root();
    let s = FileStorage::new(root.path());
    create(&s, "p", "a").await;
    let path = document(&s, "p");
    let mut json: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    json["records"][0]["position"] = "invalid".into();
    let before = serde_json::to_vec(&json).unwrap();
    std::fs::write(&path, &before).unwrap();
    match s.read(owner("p")).await.err().unwrap() {
        StoreError::Backend(orch_core::Error::Serde {
            context,
            path: reported,
            source,
        }) => {
            assert_eq!(context, "cannot parse the storage document");
            assert_eq!(reported.as_deref(), Some(path.as_path()));
            assert!(
                source
                    .to_string()
                    .contains("data did not match any variant of untagged enum StoredRecord")
            );
        }
        other => panic!("expected an inline parse error, got {other:?}"),
    }
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

#[tokio::test]
async fn retry_text_boundary_is_backed_before_the_hash_row_and_reopens_whole() {
    use crate::blob::{BlobHash, DiskBlobStore};
    use crate::data_dir::confined::ConfinedDir;
    let root = root();
    let storage = FileStorage::new(root.path());
    let inline = Retry {
        error: "é".repeat(32_768),
        ..retry("inline", 1, 10)
    };
    let row = inline.clone();
    storage
        .transaction(owner("p"), Lane::Entities, move |u| u.put_retry(&row))
        .await
        .unwrap();
    let dir = storage.project_dir("p");
    assert!(!dir.join("blobs").exists());
    let external = Retry {
        error: format!("{}!", "é".repeat(32_768)),
        ..retry("external", 2, 20)
    };
    let hash = BlobHash::of(external.error.as_bytes());
    let row = external.clone();
    let blob_dir = dir.clone();
    storage
        .transaction(owner("p"), Lane::Entities, move |u| {
            u.put_retry(&row)?;
            // No document with a hash row has committed, but its complete bytes exist.
            let document: serde_json::Value =
                serde_json::from_slice(&std::fs::read(blob_dir.join("state.json")).unwrap())
                    .unwrap();
            assert!(document["retries"]["rows"]["external"].is_null());
            let blobs = DiskBlobStore::open(&ConfinedDir::open(&blob_dir).unwrap()).unwrap();
            assert_eq!(blobs.get(&hash).unwrap(), row.error.as_bytes());
            Ok(())
        })
        .await
        .unwrap();
    let document: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.join("state.json")).unwrap()).unwrap();
    assert!(document["records"].as_array().unwrap().is_empty());
    assert_eq!(document["retries"]["rows"]["inline"]["error"], inline.error);
    assert_eq!(
        document["retries"]["rows"]["external"]["error"],
        serde_json::json!({"sha256":hash.to_string()})
    );
    drop(storage);
    let reopened = FileStorage::new(root.path());
    let snap = reopened.read(owner("p")).await.unwrap();
    assert_eq!(
        snap.read_retry("inline").await.unwrap(),
        Some(inline.clone())
    );
    assert_eq!(
        snap.read_retry("external").await.unwrap(),
        Some(external.clone())
    );
    assert_eq!(
        snap.read_due_retries(i64::MAX, 10).await.unwrap(),
        [inline.clone(), external.clone()]
    );
    assert_eq!(
        snap.read_retries(None, 10).await.unwrap().rows,
        [inline, external]
    );
}

// Only synthetic bytes and a task-local directory. The memory reference uses
// the existing blob port with the real disk adapter, not a whole-string stand-in.
fn retry_backing(
    memory: bool,
    root: &Path,
    store: impl orch_core::store::blob::BlobStore<
        Hash = crate::blob::BlobHash,
        Error = crate::blob::BlobError,
    > + 'static,
) -> std::sync::Arc<dyn Storage> {
    if memory {
        std::sync::Arc::new(orch_core::store::fake::FakeStorage::with_retry_blob_store(
            store,
            crate::blob::BlobHash::of,
            crate::blob::MAX_BLOB_BYTES as usize,
        ))
    } else {
        let mut storage = FileStorage::new(root);
        storage.retry_blobs = Some(std::sync::Arc::new(store));
        std::sync::Arc::new(storage)
    }
}

fn retry_blob_path(dir: &Path, bytes: &[u8]) -> std::path::PathBuf {
    let hash = crate::blob::BlobHash::of(bytes).to_string();
    dir.join("blobs/sha256")
        .join(&hash[..2])
        .join(&hash[2..4])
        .join(hash)
}

struct RetryAsyncReadGuard {
    store: crate::blob::DiskBlobStore,
    executor: std::thread::ThreadId,
    armed: std::sync::Arc<std::sync::atomic::AtomicBool>,
}
impl orch_core::store::blob::BlobStore for RetryAsyncReadGuard {
    type Hash = crate::blob::BlobHash;
    type Error = crate::blob::BlobError;
    fn put(&self, bytes: &[u8]) -> std::result::Result<Self::Hash, Self::Error> {
        self.store.put(bytes)
    }
    fn get(&self, hash: &Self::Hash) -> std::result::Result<Vec<u8>, Self::Error> {
        if self.armed.load(std::sync::atomic::Ordering::SeqCst) {
            assert_ne!(
                std::thread::current().id(),
                self.executor,
                "blob read on async executor"
            );
        }
        self.store.get(hash)
    }
    fn remove(&self, hash: &Self::Hash) -> std::result::Result<bool, Self::Error> {
        self.store.remove(hash)
    }
}

#[tokio::test(flavor = "current_thread")]
async fn retry_selection_does_not_resolve_excluded_damaged_rows_on_both_references() {
    use crate::blob::DiskBlobStore;
    use crate::data_dir::confined::ConfinedDir;
    for memory in [false, true] {
        let root = root();
        let dir = root.path().join("p-70");
        std::fs::create_dir(&dir).unwrap();
        let store = DiskBlobStore::open(&ConfinedDir::open(&dir).unwrap()).unwrap();
        let armed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let storage = retry_backing(
            memory,
            root.path(),
            RetryAsyncReadGuard {
                store,
                executor: std::thread::current().id(),
                armed: armed.clone(),
            },
        );
        let healthy = Retry {
            error: "h".repeat(65_537),
            ..retry("healthy", 1, 10)
        };
        let damaged = Retry {
            error: "d".repeat(65_537),
            ..retry("damaged", 1, 20)
        };
        let rows = [healthy.clone(), damaged.clone()];
        storage
            .transaction(owner("p"), Lane::Entities, move |u| {
                for row in &rows {
                    u.put_retry(row)?;
                }
                Ok(())
            })
            .await
            .unwrap();
        let snap = storage.read(owner("p")).await.unwrap();
        std::fs::remove_file(retry_blob_path(&dir, damaged.error.as_bytes())).unwrap();
        armed.store(true, std::sync::atomic::Ordering::SeqCst);
        // Deadline and page bounds must select before any excluded text is hydrated.
        assert_eq!(
            snap.read_due_retries(10, 10).await.unwrap().as_slice(),
            std::slice::from_ref(&healthy)
        );
        assert_eq!(
            snap.read_due_retries(20, 1).await.unwrap().as_slice(),
            std::slice::from_ref(&healthy)
        );
        let page = snap.read_retries(None, 1).await.unwrap();
        assert_eq!(page.rows.as_slice(), std::slice::from_ref(&healthy));
        assert_eq!(
            page.next,
            Some(orch_core::store::retries::RetryCursor::after(&healthy))
        );
        assert!(snap.read_retry("damaged").await.is_err());
        assert_eq!(
            snap.read_retry("healthy").await.unwrap(),
            Some(healthy.clone())
        );
        assert!(snap.read_retries(page.next.as_ref(), 1).await.is_err());
        armed.store(false, std::sync::atomic::Ordering::SeqCst);
        storage
            .transaction(owner("p"), Lane::Entities, move |u| {
                assert_eq!(
                    u.due_retries(10, 10)?.as_slice(),
                    std::slice::from_ref(&healthy)
                );
                assert_eq!(
                    u.due_retries(20, 1)?.as_slice(),
                    std::slice::from_ref(&healthy)
                );
                let page = u.list_retries(None, 1)?;
                assert_eq!(page.rows.as_slice(), std::slice::from_ref(&healthy));
                assert_eq!(
                    page.next,
                    Some(orch_core::store::retries::RetryCursor::after(&healthy))
                );
                assert!(u.get_retry("damaged").is_err());
                assert!(u.list_retries(page.next.as_ref(), 1).is_err());
                Ok(())
            })
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn retry_stored_text_unknown_fields_are_refused_for_single_and_chunked_objects() {
    for chunked in [false, true] {
        let root = root();
        let storage = FileStorage::new(root.path());
        storage
            .transaction(owner("p"), Lane::Entities, |u| {
                u.put_retry(&retry("t", 1, 10))
            })
            .await
            .unwrap();
        let path = root.path().join("p-70/state.json");
        let mut doc: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let mut text = serde_json::json!({"sha256": "a".repeat(64), "extra": 1});
        if chunked {
            text["chunks"] = serde_json::json!(["b".repeat(64)]);
        }
        doc["retries"]["rows"]["t"]["error"] = text;
        let before = serde_json::to_vec(&doc).unwrap();
        std::fs::write(&path, &before).unwrap();
        let reopened = FileStorage::new(root.path());
        let error = reopened
            .read(owner("p"))
            .await
            .err()
            .expect("malformed mapping must fail at load");
        assert_corruption(
            error,
            &path,
            Corruption::Parse("did not match any variant of untagged enum StoredText"),
        );
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
}

#[tokio::test]
async fn retry_missing_and_corrupt_blobs_fail_all_read_paths_on_both_references() {
    use crate::blob::DiskBlobStore;
    use crate::data_dir::confined::ConfinedDir;
    for memory in [false, true] {
        for corrupt in [false, true] {
            let root = root();
            let dir = root.path().join("p-70");
            std::fs::create_dir(&dir).unwrap();
            let store = DiskBlobStore::open(&ConfinedDir::open(&dir).unwrap()).unwrap();
            let storage = retry_backing(memory, root.path(), store);
            let row = Retry {
                error: "é".repeat(32_769),
                ..retry("damaged", 1, 10)
            };
            let bytes = row.error.as_bytes().to_vec();
            storage
                .transaction(owner("p"), Lane::Entities, move |u| u.put_retry(&row))
                .await
                .unwrap();
            let snap = storage.read(owner("p")).await.unwrap();
            let path = retry_blob_path(&dir, &bytes);
            if corrupt {
                std::fs::write(&path, b"wrong bytes").unwrap();
            } else {
                std::fs::remove_file(&path).unwrap();
            }
            // A held snapshot retains the reference; damage cannot turn it into a shortened row.
            let error = snap.read_retry("damaged").await.unwrap_err().to_string();
            assert!(error.contains("damaged"), "{error}");
            assert!(
                error.contains(if corrupt { "does not hash" } else { "no blob" }),
                "{error}"
            );
            assert!(snap.read_due_retries(i64::MAX, 10).await.is_err());
            assert!(snap.read_retries(None, 10).await.is_err());
            let document = std::fs::read(dir.join("state.json")).ok();
            storage
                .transaction(owner("p"), Lane::Entities, |u| {
                    assert!(u.get_retry("damaged").is_err());
                    assert!(u.due_retries(i64::MAX, 10).is_err());
                    assert!(u.list_retries(None, 10).is_err());
                    assert_eq!(u.get_retry("absent")?, None);
                    Ok(())
                })
                .await
                .unwrap();
            assert_eq!(std::fs::read(dir.join("state.json")).ok(), document);
        }
    }
}

// Damage the published name after the real disk put returns. The retry
// adapter must verify it before staging a reference. Neither failure may replace the staged row.
struct RetryPublicationFault {
    store: crate::blob::DiskBlobStore,
    dir: std::path::PathBuf,
    corrupt: bool,
    armed: std::sync::Arc<std::sync::atomic::AtomicBool>,
}
impl orch_core::store::blob::BlobStore for RetryPublicationFault {
    type Hash = crate::blob::BlobHash;
    type Error = crate::blob::BlobError;
    fn put(&self, bytes: &[u8]) -> std::result::Result<Self::Hash, Self::Error> {
        use std::sync::atomic::Ordering;
        let hash = self.store.put(bytes)?;
        if self.armed.load(Ordering::SeqCst) {
            let path = retry_blob_path(&self.dir, bytes);
            if self.corrupt {
                std::fs::write(path, b"wrong bytes")?;
            } else {
                std::fs::remove_file(path)?;
            }
        }
        Ok(hash)
    }
    fn get(&self, hash: &Self::Hash) -> std::result::Result<Vec<u8>, Self::Error> {
        self.store.get(hash)
    }
    fn remove(&self, hash: &Self::Hash) -> std::result::Result<bool, Self::Error> {
        self.store.remove(hash)
    }
}

#[tokio::test]
async fn retry_publication_refusal_preserves_prior_rows_even_when_caught_on_both_references() {
    use crate::blob::DiskBlobStore;
    use crate::data_dir::confined::ConfinedDir;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    for memory in [false, true] {
        for corrupt in [false, true] {
            let root = root();
            let dir = root.path().join("p-70");
            std::fs::create_dir(&dir).unwrap();
            let replacement = Retry {
                error: format!("{}!", "é".repeat(32_768)),
                ..retry("keep", 2, 20)
            };
            let armed = Arc::new(AtomicBool::new(false));
            let store = DiskBlobStore::open(&ConfinedDir::open(&dir).unwrap()).unwrap();
            let store = RetryPublicationFault {
                store,
                dir: dir.clone(),
                corrupt,
                armed: armed.clone(),
            };
            let storage = retry_backing(memory, root.path(), store);
            let previous = Retry {
                error: "old".repeat(30_000),
                ..retry("keep", 1, 10)
            };
            let old = previous.clone();
            storage
                .transaction(owner("p"), Lane::Entities, move |u| u.put_retry(&old))
                .await
                .unwrap();
            let document = std::fs::read(dir.join("state.json")).ok();
            armed.store(true, Ordering::SeqCst);
            let old = previous.clone();
            storage
                .transaction(owner("p"), Lane::Entities, move |u| {
                    let error = u.put_retry(&replacement).unwrap_err().to_string();
                    assert!(
                        error.contains(if corrupt { "does not hash" } else { "no blob" })
                            || error.contains("No such file"),
                        "{error}"
                    );
                    assert_eq!(u.get_retry("keep")?, Some(old.clone()));
                    assert_eq!(
                        u.due_retries(i64::MAX, 10)?.as_slice(),
                        std::slice::from_ref(&old)
                    );
                    assert_eq!(u.list_retries(None, 10)?.rows, [old]);
                    Ok(()) // Deliberately catch the failure and commit.
                })
                .await
                .unwrap();
            assert_eq!(std::fs::read(dir.join("state.json")).ok(), document);
            assert_eq!(
                storage
                    .read(owner("p"))
                    .await
                    .unwrap()
                    .read_retry("keep")
                    .await
                    .unwrap(),
                Some(previous)
            );
        }
    }
}

#[tokio::test]
async fn retry_object_limit_is_not_a_diagnostic_cap_and_chunk_order_is_verified() {
    use crate::blob::{BlobHash, DiskBlobStore, MAX_BLOB_BYTES};
    use crate::data_dir::confined::ConfinedDir;
    let max = MAX_BLOB_BYTES as usize;
    for memory in [false, true] {
        for extra in [false, true] {
            let root = root();
            let dir = root.path().join("p-70");
            std::fs::create_dir(&dir).unwrap();
            let store = DiskBlobStore::open(&ConfinedDir::open(&dir).unwrap()).unwrap();
            let storage = retry_backing(memory, root.path(), store);
            let mut error = "€".repeat(max / 3); // 67,108,863 bytes.
            error.push_str(if extra { "é" } else { "!" });
            assert_eq!(error.len(), max + usize::from(extra));
            let hash = BlobHash::of(error.as_bytes());
            let row = Retry {
                error,
                ..retry("large", 1, 10)
            };
            let expected = row.clone();
            storage
                .transaction(owner("p"), Lane::Entities, move |u| {
                    u.put_retry(&row)?;
                    assert_eq!(u.get_retry("large")?, Some(row));
                    Ok(())
                })
                .await
                .unwrap();
            let snap = storage.read(owner("p")).await.unwrap();
            assert_eq!(
                snap.read_retry("large").await.unwrap(),
                Some(expected.clone())
            );
            assert_eq!(
                snap.read_due_retries(i64::MAX, 1).await.unwrap().as_slice(),
                std::slice::from_ref(&expected)
            );
            assert_eq!(
                snap.read_retries(None, 1).await.unwrap().rows.as_slice(),
                std::slice::from_ref(&expected)
            );
            let blobs = DiskBlobStore::open(&ConfinedDir::open(&dir).unwrap()).unwrap();
            if extra {
                assert!(!retry_blob_path(&dir, expected.error.as_bytes()).exists());
                let mut combined = Vec::new();
                for chunk in expected.error.as_bytes().chunks(max) {
                    let bytes = blobs.get(&BlobHash::of(chunk)).unwrap();
                    assert_eq!(bytes, chunk);
                    // The two-byte character straddles the object boundary.
                    assert!(std::str::from_utf8(&bytes).is_err());
                    combined.extend(bytes);
                }
                assert_eq!(BlobHash::of(&combined), hash);
                assert_eq!(combined, expected.error.as_bytes());
                let tail = &expected.error.as_bytes()[max..];
                let path = retry_blob_path(&dir, tail);
                for corrupt in [true, false] {
                    if corrupt {
                        std::fs::write(&path, b"damaged tail").unwrap();
                    } else {
                        std::fs::remove_file(&path).unwrap();
                    }
                    assert_retry_reads_fail(storage.as_ref(), snap.as_ref(), "large").await;
                    std::fs::write(&path, tail).unwrap();
                }
                if !memory {
                    let path = dir.join("state.json");
                    let mut document: serde_json::Value =
                        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
                    let mapping = &mut document["retries"]["rows"]["large"]["error"];
                    assert_eq!(mapping["sha256"], hash.to_string());
                    let chunks = mapping["chunks"].as_array_mut().unwrap();
                    assert_eq!(chunks.len(), 2);
                    chunks.swap(0, 1);
                    std::fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();
                    let reopened = FileStorage::new(root.path());
                    let snap = reopened.read(owner("p")).await.unwrap();
                    let error = snap.read_retry("large").await.unwrap_err().to_string();
                    assert!(
                        error.contains("reconstructed retry does not hash"),
                        "{error}"
                    );
                    assert_retry_reads_fail(&reopened, snap.as_ref(), "large").await;
                }
            } else {
                assert_eq!(blobs.get(&hash).unwrap(), expected.error.as_bytes());
            }
        }
    }
}

#[tokio::test]
async fn retry_later_chunk_failure_keeps_prior_row_and_earlier_published_bytes() {
    use crate::blob::{BlobHash, DiskBlobStore, MAX_BLOB_BYTES};
    use crate::data_dir::confined::ConfinedDir;
    for memory in [false, true] {
        let root = root();
        let dir = root.path().join("p-70");
        std::fs::create_dir(&dir).unwrap();
        let store = DiskBlobStore::open(&ConfinedDir::open(&dir).unwrap()).unwrap();
        let storage = retry_backing(memory, root.path(), store);
        let old = retry("keep", 1, 10);
        let previous = old.clone();
        storage
            .transaction(owner("p"), Lane::Entities, move |u| u.put_retry(&previous))
            .await
            .unwrap();
        let document = std::fs::read(dir.join("state.json")).ok();
        let max = MAX_BLOB_BYTES as usize;
        let first = "x".repeat(max);
        let last = b"!";
        let hash = BlobHash::of(first.as_bytes());
        let path = retry_blob_path(&dir, last);
        // The second object's shard is unusable; the first can publish normally.
        std::fs::create_dir_all(path.parent().unwrap().parent().unwrap()).unwrap();
        std::fs::write(path.parent().unwrap(), b"blocked shard").unwrap();
        let new = Retry {
            error: format!("{first}!"),
            ..retry("keep", 2, 20)
        };
        let previous = old.clone();
        storage
            .transaction(owner("p"), Lane::Entities, move |u| {
                assert!(u.put_retry(&new).is_err());
                assert_eq!(u.get_retry("keep")?, Some(previous));
                // Other work commits even though the replacement failed.
                u.put_retry(&retry("other", 1, 30))
            })
            .await
            .unwrap();
        assert_eq!(
            storage
                .read(owner("p"))
                .await
                .unwrap()
                .read_retry("keep")
                .await
                .unwrap(),
            Some(old)
        );
        let blobs = DiskBlobStore::open(&ConfinedDir::open(&dir).unwrap()).unwrap();
        assert_eq!(blobs.get(&hash).unwrap(), first.as_bytes());
        if let Some(document) = document {
            let before: serde_json::Value = serde_json::from_slice(&document).unwrap();
            let after: serde_json::Value =
                serde_json::from_slice(&std::fs::read(dir.join("state.json")).unwrap()).unwrap();
            assert_eq!(
                before["retries"]["rows"]["keep"],
                after["retries"]["rows"]["keep"]
            );
            assert_eq!(after["retries"]["rows"].as_object().unwrap().len(), 2);
        }
    }
}

#[tokio::test]
async fn retry_repeated_chunks_reconstruct_every_occurrence_on_both_references() {
    use crate::blob::{BlobHash, DiskBlobStore, MAX_BLOB_BYTES};
    use crate::data_dir::confined::ConfinedDir;
    for memory in [false, true] {
        let root = root();
        let dir = root.path().join("p-70");
        std::fs::create_dir(&dir).unwrap();
        let store = DiskBlobStore::open(&ConfinedDir::open(&dir).unwrap()).unwrap();
        let storage = retry_backing(memory, root.path(), store);
        let chunk = "x".repeat(MAX_BLOB_BYTES as usize);
        let hash = BlobHash::of(chunk.as_bytes());
        let row = Retry {
            error: format!("{chunk}{chunk}!"),
            ..retry("repeated", 1, 10)
        };
        let expected = row.clone();
        storage
            .transaction(owner("p"), Lane::Entities, move |u| u.put_retry(&row))
            .await
            .unwrap();
        assert_eq!(
            storage
                .read(owner("p"))
                .await
                .unwrap()
                .read_retry("repeated")
                .await
                .unwrap(),
            Some(expected)
        );
        let blobs = DiskBlobStore::open(&ConfinedDir::open(&dir).unwrap()).unwrap();
        assert_eq!(blobs.get(&hash).unwrap(), chunk.as_bytes());
        if !memory {
            let document: serde_json::Value =
                serde_json::from_slice(&std::fs::read(dir.join("state.json")).unwrap()).unwrap();
            let chunks = document["retries"]["rows"]["repeated"]["error"]["chunks"]
                .as_array()
                .unwrap();
            assert_eq!(chunks.len(), 3);
            assert_eq!(chunks[0], hash.to_string());
            assert_eq!(chunks[1], chunks[0]);
        }
    }
}

async fn assert_retry_reads_fail(storage: &dyn Storage, snap: &dyn ReadSnapshot, task: &str) {
    assert!(snap.read_retry(task).await.is_err());
    assert!(snap.read_due_retries(i64::MAX, 10).await.is_err());
    assert!(snap.read_retries(None, 10).await.is_err());
    let task = task.to_owned();
    storage
        .transaction(owner("p"), Lane::Entities, move |u| {
            assert!(u.get_retry(&task).is_err());
            assert!(u.due_retries(i64::MAX, 10).is_err());
            assert!(u.list_retries(None, 10).is_err());
            Ok(())
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn retry_rollback_keeps_published_bytes_without_a_reference_on_both_backed_references() {
    use crate::blob::DiskBlobStore;
    use crate::data_dir::confined::ConfinedDir;
    for memory in [false, true] {
        let root = root();
        let dir = root.path().join("p-70");
        std::fs::create_dir(&dir).unwrap();
        let store = DiskBlobStore::open(&ConfinedDir::open(&dir).unwrap()).unwrap();
        let storage = retry_backing(memory, root.path(), store);
        let old = retry("keep", 1, 10);
        let previous = old.clone();
        storage
            .transaction(owner("p"), Lane::Entities, move |u| u.put_retry(&previous))
            .await
            .unwrap();
        let row = Retry {
            error: "x".repeat(65_537),
            ..retry("lost", 1, 20)
        };
        let bytes = row.error.as_bytes().to_vec();
        let result = storage
            .transaction(owner("p"), Lane::Entities, move |u| {
                u.put_retry(&row)?;
                Err::<(), _>(StoreError::CounterExhausted)
            })
            .await;
        assert!(matches!(result, Err(StoreError::CounterExhausted)));
        let snap = storage.read(owner("p")).await.unwrap();
        assert_eq!(snap.read_retries(None, 10).await.unwrap().rows, [old]);
        assert_eq!(snap.read_retry("lost").await.unwrap(), None);
        assert_eq!(std::fs::read(retry_blob_path(&dir, &bytes)).unwrap(), bytes);
    }
}

#[tokio::test]
async fn retry_existing_single_object_encoding_and_wrapper_extensions_remain_readable() {
    use crate::blob::{BlobHash, DiskBlobStore};
    use crate::data_dir::confined::ConfinedDir;
    let root = root();
    let storage = FileStorage::new(root.path());
    store_key(&storage, "first", "r", 1).await;
    storage
        .transaction(owner("p"), Lane::Entities, |u| {
            u.put_retry(&retry("legacy", 1, 10))
        })
        .await
        .unwrap();
    let dir = storage.project_dir("p");
    let path = dir.join("state.json");
    let bytes = "é".repeat(32_769).into_bytes();
    let blobs = DiskBlobStore::open(&ConfinedDir::open(&dir).unwrap()).unwrap();
    let hash = blobs.put(&bytes).unwrap();
    let mut doc: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    doc["retries"]["rows"]["legacy"]["error"] = serde_json::json!({"sha256": hash.to_string()});
    doc["request_keys"]["future-wrapper-field"] = serde_json::json!({"ignored":true});
    std::fs::write(&path, serde_json::to_vec(&doc).unwrap()).unwrap();
    let before = std::fs::read(&path).unwrap();
    let reopened = FileStorage::new(root.path());
    let snap = reopened.read(owner("p")).await.unwrap();
    let expected = Retry {
        error: String::from_utf8(bytes).unwrap(),
        ..retry("legacy", 1, 10)
    };
    assert_eq!(
        snap.read_retry("legacy").await.unwrap(),
        Some(expected.clone())
    );
    assert_eq!(
        snap.read_due_retries(i64::MAX, 10)
            .await
            .unwrap()
            .as_slice(),
        std::slice::from_ref(&expected)
    );
    assert_eq!(snap.read_retries(None, 10).await.unwrap().rows, [expected]);
    assert_eq!(BlobHash::of("é".repeat(32_769).as_bytes()), hash);
    assert_eq!(
        check_key(&reopened, "first", 1).await,
        KeyCheck::Replay("r".into())
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
}
