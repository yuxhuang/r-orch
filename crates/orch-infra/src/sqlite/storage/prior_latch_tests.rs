//! Reads and writes after a latch that was set before them, through the public
//! `Storage` port (issue 389; spec R128, R140).
//!
//! The policy under test: a latched state refuses every write and serves a new
//! snapshot; a snapshot answers no call after a fatal observation that was made
//! since it was requested, whichever connection made it. The latch is set from the
//! checkpoint source, so that no connection of the reads below saw damage.
use super::*;
use crate::sqlite::{ConnectionRole, FatalCause, Synchronous, open_connection};
use orch_core::store::attention::{AttentionFacts, AttentionFilter};
use orch_core::store::leases::{ClaimOutcome, NewLease};
use orch_core::store::{Lane, Principal, StorageExt};
use std::sync::atomic::AtomicUsize;

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
fn unavailable<T>(result: store::Result<T>) -> bool {
    matches!(result, Err(e) if e.to_string().contains("storage_unavailable"))
}
fn latch(storage: &SqliteStorage) {
    storage
        .failure_state()
        .latch(FailureSource::Checkpoint, FatalCause::SqliteCorrupt);
}

/// One team, one lease and one attention item: every read has a value to return.
async fn seeded() -> (tempfile::TempDir, Arc<DataDir>, SqliteStorage) {
    let (temp, data) = crate::sqlite::test_data();
    let data = Arc::new(data);
    let storage = SqliteStorage::open(data.clone(), StorageOptions::default()).unwrap();
    storage
        .transaction(owner(), Lane::Entities, |u| {
            u.create(team("kept"))?;
            let ClaimOutcome::Granted(_) = u.claim_lease(
                &NewLease {
                    task: "t".into(),
                    agent: "a".into(),
                    run: "r".into(),
                },
                0,
            )?
            else {
                panic!("grant")
            };
            u.open_attention(
                &AttentionFacts::DispatchOffWithWork {
                    waiting_tasks: 1,
                    oldest_waiting_at: 1,
                },
                1,
            )?;
            Ok(())
        })
        .await
        .unwrap();
    (temp, data, storage)
}

#[tokio::test]
async fn a_prior_latch_serves_a_new_snapshot_entity_and_operational_reads() {
    let (_temp, _data, storage) = seeded().await;
    latch(&storage);
    let snapshot = storage
        .read(owner())
        .await
        .expect("a latch refuses no read");
    // Several calls: the snapshot must stay open after the first one.
    for _ in 0..2 {
        assert!(
            snapshot
                .get(EntityKind::Team, "kept")
                .await
                .unwrap()
                .is_some()
        );
        let teams = snapshot
            .list(&Filter::Teams, &PageRequest::default())
            .await
            .unwrap();
        assert_eq!(teams.items.len(), 1);
        assert!(snapshot.read_lease("t").await.unwrap().is_some());
        assert_eq!(snapshot.read_leases().await.unwrap().len(), 1);
        assert_eq!(
            snapshot
                .read_attention_page(AttentionFilter::Open, None, 10)
                .await
                .unwrap()
                .items
                .len(),
            1
        );
    }
    assert_eq!(
        storage.failure_state().failure().unwrap().source,
        FailureSource::Checkpoint,
        "serving reads neither changes nor clears the latch"
    );
    drop(snapshot);
    storage.close().await.unwrap();
}

#[tokio::test]
async fn a_fatal_observation_since_a_snapshot_was_requested_refuses_its_calls() {
    let (_temp, _data, storage) = seeded().await;
    let before = storage.read(owner()).await.unwrap();
    assert!(
        before
            .get(EntityKind::Team, "kept")
            .await
            .unwrap()
            .is_some()
    );
    latch(&storage);
    // It pinned its view before the fatal observation: no value is returned.
    assert!(unavailable(before.read_leases().await));
    // The refusal ends that snapshot, as any failed call does.
    assert!(before.get(EntityKind::Team, "kept").await.is_err());
    // A snapshot requested after the latch is served.
    let after = storage.read(owner()).await.unwrap();
    assert!(after.get(EntityKind::Team, "kept").await.unwrap().is_some());
    // And a second, later fatal observation, with the state already latched, refuses it.
    storage
        .failure_state()
        .observe(FailureSource::Reader, &fatal_sql());
    assert!(unavailable(after.get(EntityKind::Team, "kept").await));
    let later = storage.read(owner()).await.unwrap();
    assert!(later.get(EntityKind::Team, "kept").await.unwrap().is_some());
    drop((before, after, later));
    storage.close().await.unwrap();
}
fn fatal_sql() -> SqliteError {
    SqliteError::Sql(rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CORRUPT),
        None,
    ))
}

#[tokio::test]
async fn a_read_that_finds_damage_after_a_prior_latch_returns_no_value_and_is_counted() {
    let (_temp, data, storage) = seeded().await;
    // Damage the stored detail of the team behind the adapter's back.
    open_connection(&data, ConnectionRole::Writer, Synchronous::Normal)
        .unwrap()
        .execute(
            "DELETE FROM entity_details WHERE project_id='p' AND family='team'",
            [],
        )
        .unwrap();
    latch(&storage);
    let state = storage.failure_state();
    let counted = state.fatal_observations();
    let snapshot = storage.read(owner()).await.unwrap();
    // Served until the read itself meets the damage: no value, and not a refusal
    // for the prior latch: this refusal is for the freshly observed damage.
    let damaged = snapshot.get(EntityKind::Team, "kept").await;
    assert!(
        matches!(&damaged, Err(e) if e.to_string().contains("storage_unavailable")),
        "{damaged:?}"
    );
    assert!(
        state.fatal_observations() > counted,
        "a fresh fatal observation counts although the state was already latched"
    );
    assert_eq!(state.failure().unwrap().source, FailureSource::Checkpoint);
    // The damaged read ended its snapshot; the next one starts after the observation.
    let next = storage.read(owner()).await.unwrap();
    assert_eq!(next.read_leases().await.unwrap().len(), 1);
    drop((snapshot, next));
    storage.close().await.unwrap();
}

#[tokio::test]
async fn a_prior_latch_refuses_every_write_and_never_runs_its_callback() {
    let (_temp, _data, storage) = seeded().await;
    latch(&storage);
    let ran = Arc::new(AtomicUsize::new(0));
    for lane in [
        Lane::Entities,
        Lane::Claims,
        Lane::Events,
        Lane::Maintenance,
    ] {
        let seen = ran.clone();
        let refused = storage
            .write(
                owner(),
                lane,
                Box::new(move |u| {
                    seen.fetch_add(1, Ordering::SeqCst);
                    u.create(team("refused")).map(|_| ())
                }),
            )
            .await;
        assert!(unavailable(refused), "{lane:?}");
    }
    assert_eq!(ran.load(Ordering::SeqCst), 0);
    let snapshot = storage.read(owner()).await.unwrap();
    assert!(
        snapshot
            .get(EntityKind::Team, "refused")
            .await
            .unwrap()
            .is_none()
    );
    drop(snapshot);
    storage.close().await.unwrap();
}

#[tokio::test]
async fn a_latch_during_a_writer_callback_refuses_its_later_work_and_its_commit() {
    let (_temp, _data, storage) = seeded().await;
    let state = storage.failure_state();
    let observed = Arc::new(AtomicUsize::new(0));
    let seen = observed.clone();
    let result = storage
        .write(
            owner(),
            Lane::Entities,
            Box::new(move |u| {
                u.create(team("must-rollback"))?;
                state.latch(FailureSource::Checkpoint, FatalCause::SqliteCorrupt);
                // A snapshot read is served in the same incident (see above); the unit is not.
                for refused in [
                    u.get(EntityKind::Team, "kept").is_err(),
                    u.list(&Filter::Teams, &PageRequest::default()).is_err(),
                    u.create(team("refused")).is_err(),
                    u.list_leases().is_err(),
                ] {
                    assert!(refused);
                    seen.fetch_add(1, Ordering::SeqCst);
                }
                // A callback that swallows every refusal must still not commit.
                Ok(())
            }),
        )
        .await;
    assert!(unavailable(result));
    assert_eq!(observed.load(Ordering::SeqCst), 4);
    let snapshot = storage.read(owner()).await.unwrap();
    assert!(
        snapshot
            .get(EntityKind::Team, "kept")
            .await
            .unwrap()
            .is_some()
    );
    for gone in ["must-rollback", "refused"] {
        assert!(
            snapshot
                .get(EntityKind::Team, gone)
                .await
                .unwrap()
                .is_none()
        );
    }
    drop(snapshot);
    storage.close().await.unwrap();
}
