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
async fn owned_snapshot_pins_at_open_and_outlives_storage_handle() {
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
    // A snapshot is independently owned; it keeps the owner/lock alive until
    // explicit close or the final handle is dropped.
    assert!(
        snapshot
            .get(EntityKind::Team, "later")
            .await
            .unwrap()
            .is_none()
    );
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
