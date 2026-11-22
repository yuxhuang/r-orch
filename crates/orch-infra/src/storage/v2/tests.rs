//! What only a file reference can get wrong: the document on disk. The shared
//! scenarios in `orch-engine`'s `storage_conformance` cover the port's rules.

use std::path::Path;

use orch_core::store::events::{EventGap, SequencedEvent};
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
