use super::*;
use orch_core::store::{Principal, Storage, StorageExt};

fn owner() -> Caller {
    Caller {
        project: "p".into(),
        principal: Principal::Owner {
            identity: "synthetic".into(),
        },
    }
}
fn team(id: &str) -> Entity {
    Entity::Team(Box::new(serde_json::from_value(serde_json::json!({"id":id,"name":id,"status":"active","members":[],"task_pool":[],"lead_agent_id":"a","created_at":"opaque","updated_at":"opaque","config":{"auto_claim":false}})).unwrap()))
}
#[tokio::test]
async fn owned_snapshot_pins_at_open_and_stops_with_last_storage_handle() {
    let (_temp, data) = crate::sqlite::test_data();
    let storage = SqliteStorage::open(Arc::new(data), StorageOptions::default()).unwrap();
    let snapshot = storage.read(owner()).await.unwrap();
    storage
        .transaction(owner(), orch_core::store::Lane::Entities, |u| {
            u.create(team("later"))
        })
        .await
        .unwrap();
    assert!(
        snapshot
            .get(EntityKind::Team, "later")
            .await
            .unwrap()
            .is_none()
    );
    let newer = storage.read(owner()).await.unwrap();
    assert!(
        newer
            .get(EntityKind::Team, "later")
            .await
            .unwrap()
            .is_some()
    );
    drop(storage);
    assert!(snapshot.get(EntityKind::Team, "later").await.is_err());
}
#[tokio::test]
async fn callback_panic_is_resumed_and_the_following_write_commits() {
    let (_temp, data) = crate::sqlite::test_data();
    let storage = SqliteStorage::open(Arc::new(data), StorageOptions::default()).unwrap();
    let other = storage.clone();
    let result = tokio::spawn(async move {
        other
            .write(
                owner(),
                orch_core::store::Lane::Entities,
                Box::new(|u| {
                    u.create(team("rolled-back"))?;
                    panic!("synthetic callback panic")
                }),
            )
            .await
    })
    .await;
    assert!(result.unwrap_err().is_panic());
    storage
        .transaction(owner(), orch_core::store::Lane::Entities, |u| {
            u.create(team("kept"))
        })
        .await
        .unwrap();
    let snapshot = storage.read(owner()).await.unwrap();
    assert!(
        snapshot
            .get(EntityKind::Team, "rolled-back")
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        snapshot
            .get(EntityKind::Team, "kept")
            .await
            .unwrap()
            .is_some()
    );
    assert!(storage.failure_state().failure().is_none());
    drop(snapshot);
    storage.close().await.unwrap();
}

#[tokio::test]
async fn idle_snapshot_releases_the_only_reader_and_reports_expiry() {
    let (_temp, data) = crate::sqlite::test_data();
    let storage = SqliteStorage::open(
        Arc::new(data),
        StorageOptions {
            read_connections: 1,
            snapshot_idle: Duration::from_millis(30),
            ..Default::default()
        },
    )
    .unwrap();
    let expired = storage.read(owner()).await.unwrap();
    // Opening this snapshot waits for the first worker's idle bound. There is
    // no sleep or unrelated await while an active snapshot is intentionally held.
    let next = storage.read(owner()).await.unwrap();
    assert!(
        expired
            .get(EntityKind::Team, "absent")
            .await
            .unwrap_err()
            .to_string()
            .contains("snapshot_expired")
    );
    assert!(
        next.get(EntityKind::Team, "absent")
            .await
            .unwrap()
            .is_none()
    );
    storage.close().await.unwrap();
}

#[tokio::test]
async fn fatal_reader_observation_prevents_any_later_snapshot_value() {
    let (_temp, data) = crate::sqlite::test_data();
    let storage = SqliteStorage::open(Arc::new(data), StorageOptions::default()).unwrap();
    let snapshot = storage.read(owner()).await.unwrap();
    assert!(
        snapshot
            .get(EntityKind::Team, "absent")
            .await
            .unwrap()
            .is_none()
    );
    let error = SqliteError::Sql(rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CORRUPT),
        None,
    ));
    storage
        .failure_state()
        .observe(FailureSource::Reader, &error);
    assert!(snapshot.get(EntityKind::Team, "absent").await.is_err());
    assert!(storage.read(owner()).await.is_err());
    storage.close().await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn close_ends_a_live_snapshot_and_releases_the_directory_lock() {
    use crate::data_dir::{DataDir, Holder, LockKind};
    let (temp, data) = crate::sqlite::test_data();
    let storage = SqliteStorage::open(Arc::new(data), StorageOptions::default()).unwrap();
    let snapshot = storage.read(owner()).await.unwrap();
    storage.close().await.unwrap();
    assert!(snapshot.get(EntityKind::Team, "absent").await.is_err());
    let second = DataDir::acquire(
        &temp.path().join("data"),
        &Holder {
            kind: LockKind::Daemon,
            pid: std::process::id(),
            started_at: "synthetic second".into(),
            binary_version: "test".into(),
            executable: "/synthetic/orch".into(),
        },
    )
    .unwrap();
    drop(second);
}

#[tokio::test]
async fn layout_order_and_leaf_pages_agree_for_every_non_task_shape() {
    use super::super::non_task_cursor::Layout;
    use orch_core::store::{ConfigEntry, Sort, TemplateEntry};
    use orch_core::test_support::{make_agent, make_run};
    use orch_domain::{Goal, GoalStatus};
    let (_temp, data) = crate::sqlite::test_data();
    let storage = SqliteStorage::open(Arc::new(data), StorageOptions::default()).unwrap();
    storage.transaction(owner(),store::Lane::Entities,|u| {
        for (index,id,name) in [(0,"b","é"),(1,"a","a"),(2,"c","a")] {
            let mut agent=make_agent(); agent.id=id.into();agent.name=name.into();
            u.create(Entity::Agent(Box::new(agent)))?;
            let Entity::Team(mut t)=team(id) else { unreachable!() };t.name=name.into();t.created_at=name.into();
            u.create(Entity::Team(t))?;
            let mut run=make_run();run.id=id.into();run.task_id="t".into();run.agent_id="a".into();run.attempt=[2,10,10][index];run.started_at=name.into();
            u.create(Entity::Run(Box::new(run)))?;
            u.create(Entity::Message(Box::new(serde_json::from_value(serde_json::json!({"id":id,"channel":"direct","from_agent_id":"sender","to_agent_id":"recipient","subject":"s","body":"b","created_at":name,"status":"pending"})).unwrap())))?;
            u.create(Entity::Goal(Box::new(Goal { id:id.into(),title:"title".into(),description:"description".into(),status:[GoalStatus::Active,GoalStatus::Paused,GoalStatus::Active][index],assignee:None,created_at:name.into(),updated_at:[None,Some(""),Some("z")][index].map(str::to_owned) })))?;
            u.create(Entity::Context(orch_core::storage::ContextEntry { key:id.into(),value:"v".into(),created_at:"opaque".into(),updated_at:"opaque".into(),ttl_ms:None,expires_at:None }))?;
            u.create(Entity::Config(ConfigEntry { key:id.into(),value:"v".into() }))?;
            u.create(Entity::Template(TemplateEntry { name:id.into(),body:"v".into() }))?;
        }
        Ok(())
    }).await.unwrap();
    let snapshot = storage.read(owner()).await.unwrap();
    for filter in [
        Filter::Agents,
        Filter::Teams,
        Filter::Runs {
            task: None,
            agent: None,
        },
        Filter::Runs {
            task: Some("t".into()),
            agent: Some("a".into()),
        },
        Filter::Messages {
            recipient: Some("recipient".into()),
        },
        Filter::PendingMessages {
            recipient: "recipient".into(),
            now: "z".into(),
        },
        Filter::Goals { status: None },
        Filter::Context,
        Filter::Config,
        Filter::Templates,
    ] {
        for sort in [
            Sort::Default,
            Sort::Id,
            Sort::Position,
            Sort::Name,
            Sort::Priority,
            Sort::CreatedAt,
            Sort::UpdatedAt,
            Sort::Attempt,
        ] {
            let Some(layout) = Layout::of(&filter, sort) else {
                continue;
            };
            let request = PageRequest {
                limit: 1000,
                sort,
                with_count: true,
                ..Default::default()
            };
            let all = snapshot.list(&filter, &request).await.unwrap();
            assert_eq!(all.items.len(), 3, "{filter:?} {sort:?}");
            for rows in all.items.windows(2) {
                let a = page::tuple(&filter, sort, &rows[0]).unwrap();
                let b = page::tuple(&filter, sort, &rows[1]).unwrap();
                assert!(layout.compare(&a, &b).is_lt(), "{filter:?} {sort:?}");
            }
            let mut request = PageRequest {
                limit: 1,
                ..request
            };
            let mut walked = Vec::new();
            loop {
                let one = snapshot.list(&filter, &request).await.unwrap();
                walked.extend(one.items);
                let Some(cursor) = one.next_cursor else { break };
                request.cursor = Some(cursor);
            }
            assert_eq!(walked, all.items, "{filter:?} {sort:?}");
        }
    }
    storage.close().await.unwrap();
}

#[tokio::test]
async fn a_queued_snapshot_is_refused_at_admission_deadline() {
    let (_temp, data) = crate::sqlite::test_data();
    let storage = SqliteStorage::open(
        Arc::new(data),
        StorageOptions {
            read_connections: 1,
            ..Default::default()
        },
    )
    .unwrap();
    let held = storage.read(owner()).await.unwrap();
    let refused = tokio::time::timeout(Duration::from_millis(500), storage.read(owner())).await;
    assert!(
        matches!(refused,Ok(Err(ref e)) if e.to_string().contains("overloaded")),
        "queued read must answer overload before the holder's idle bound"
    );
    assert!(
        held.get(EntityKind::Team, "missing")
            .await
            .unwrap()
            .is_none()
    );
    storage.close().await.unwrap();
}
