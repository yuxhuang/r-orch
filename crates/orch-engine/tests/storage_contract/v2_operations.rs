//! Combined event/lease boundaries on the transactional memory reference.

use orch_core::store::{events::SequencedEvent, fake::FakeStorage, leases::*, *};
use orch_domain::{RunEvent, RunEventType};

fn owner(project: &str) -> Caller {
    Caller {
        project: project.into(),
        principal: Principal::Owner {
            identity: "synthetic".into(),
        },
    }
}

fn new(run: &str) -> NewLease {
    NewLease {
        task: "task".into(),
        agent: "agent".into(),
        run: run.into(),
    }
}

fn event(seq: u64) -> SequencedEvent {
    SequencedEvent {
        seq,
        event: RunEvent {
            timestamp: "2026-01-01T00:00:00Z".into(),
            event_type: RunEventType::AgentOutput,
            data: serde_json::json!({"seq": seq}),
        },
    }
}

async fn claim(storage: &FakeStorage, run: &'static str) -> Lease {
    storage
        .transaction(owner("p"), Lane::Claims, move |unit| {
            let ClaimOutcome::Granted(lease) = unit.claim_lease(&new(run), 0)? else {
                panic!("fresh claim blocked");
            };
            Ok(lease)
        })
        .await
        .unwrap()
}

#[tokio::test]
async fn claim_and_events_commit_or_roll_back_with_entities() {
    let storage = FakeStorage::default();
    let result = storage
        .transaction(owner("p"), Lane::Claims, |unit| {
            unit.create(Entity::Task(Box::new(crate::contract::task("task"))))?;
            let ClaimOutcome::Granted(lease) = unit.claim_lease(&new("run"), 0)? else {
                panic!("fresh claim blocked");
            };
            unit.append_events("run", &lease.lineage, lease.epoch, &[event(1)])?;
            Err::<(), _>(StoreError::CounterExhausted)
        })
        .await;
    assert!(matches!(result, Err(StoreError::CounterExhausted)));
    let before = storage.read(owner("p")).await.unwrap();
    assert!(
        before
            .get(EntityKind::Task, "task")
            .await
            .unwrap()
            .is_none()
    );
    assert!(before.read_leases().await.unwrap().is_empty());
    assert!(before.read_events("run", 0, 10).await.unwrap().is_empty());
    let lease = claim(&storage, "run").await;
    assert_eq!(lease.epoch, 1);
    storage
        .transaction(owner("p"), Lane::Events, move |unit| {
            unit.append_events("run", &lease.lineage, lease.epoch, &[event(1)])?;
            Ok(())
        })
        .await
        .unwrap();
    // The old read retains both the old event rows and the old lease view.
    assert!(before.read_leases().await.unwrap().is_empty());
    assert!(before.read_events("run", 0, 10).await.unwrap().is_empty());
    let after = storage.read(owner("p")).await.unwrap();
    assert_eq!(
        after.read_events("run", 0, 10).await.unwrap(),
        vec![event(1)]
    );
}

#[tokio::test]
async fn release_immediately_fences_append_even_within_the_same_transaction() {
    let storage = FakeStorage::default();
    let lease = claim(&storage, "run").await;
    storage
        .transaction(owner("p"), Lane::Claims, move |unit| {
            unit.append_events("run", &lease.lineage, lease.epoch, &[event(1)])?;
            unit.release_lease("task", &lease.lineage, lease.epoch)?;
            for batch in [vec![], vec![event(1)], vec![event(2)]] {
                assert!(matches!(
                    unit.append_events("run", &lease.lineage, lease.epoch, &batch),
                    Err(StoreError::StaleEpoch)
                ));
            }
            Ok(())
        })
        .await
        .unwrap();
    let view = storage.read(owner("p")).await.unwrap();
    assert!(view.read_leases().await.unwrap().is_empty());
    assert_eq!(
        view.read_events("run", 0, 10).await.unwrap(),
        vec![event(1)]
    );
}

#[tokio::test]
async fn restore_refuses_old_append_without_changing_entities_events_gaps_or_leases() {
    let storage = FakeStorage::default();
    let old = claim(&storage, "run").await;
    let token = old.clone();
    storage
        .transaction(owner("p"), Lane::Events, move |unit| {
            unit.create(Entity::Task(Box::new(crate::contract::task("task"))))?;
            unit.append_events("run", &token.lineage, token.epoch, &[event(1)])?;
            unit.record_event_gap("run", 1, 1)
        })
        .await
        .unwrap();
    storage.restore_to_lineage("new-lineage").await;
    let current = claim(&storage, "replacement").await;
    assert_eq!(current.epoch, old.epoch + 1);
    let before = storage.read(owner("p")).await.unwrap();
    for caller in [
        owner("p"),
        Caller {
            project: "p".into(),
            principal: Principal::Task {
                identity: "synthetic".into(),
                task: old.task.clone(),
                run: old.run.clone(),
                lineage: old.lineage.clone(),
                epoch: old.epoch,
            },
        },
    ] {
        let token = old.clone();
        assert!(matches!(
            storage
                .transaction(caller, Lane::Events, move |unit| {
                    unit.delete(EntityKind::Task, "task", Revision::ONE)?;
                    unit.record_event_gap("run", 2, 1)?;
                    unit.append_events("run", &token.lineage, token.epoch, &[event(2)])?;
                    Ok(())
                })
                .await,
            Err(StoreError::StaleEpoch)
        ));
    }
    let after = storage.read(owner("p")).await.unwrap();
    assert_eq!(
        after.get(EntityKind::Task, "task").await.unwrap(),
        before.get(EntityKind::Task, "task").await.unwrap()
    );
    assert_eq!(after.read_leases().await.unwrap(), vec![current]);
    assert_eq!(
        after.read_events("run", 0, 10).await.unwrap(),
        vec![event(1)]
    );
    assert_eq!(
        after.read_event_gaps("run").await.unwrap(),
        before.read_event_gaps("run").await.unwrap()
    );
}

#[tokio::test]
async fn a_clock_jump_keeps_the_supervised_lease_and_event_authority() {
    let storage = FakeStorage::default();
    let lease = claim(&storage, "run").await;
    let resumed_at = lease.expires_at + 86_400;
    storage
        .transaction(owner("p"), Lane::Claims, move |unit| {
            // A rival cannot reap a supervised run solely because time advanced.
            assert_eq!(
                unit.claim_lease(&new("rival"), resumed_at)?,
                ClaimOutcome::Held(lease.clone())
            );
            let renewed = unit.renew_lease("task", &lease.lineage, lease.epoch, resumed_at)?;
            assert_eq!(renewed.expires_at, resumed_at + LEASE_SECS);
            unit.append_events("run", &lease.lineage, lease.epoch, &[event(1)])?;
            Ok(())
        })
        .await
        .unwrap();
    let view = storage.read(owner("p")).await.unwrap();
    assert_eq!(
        view.read_events("run", 0, 10).await.unwrap(),
        vec![event(1)]
    );
    assert_eq!(view.read_leases().await.unwrap().len(), 1);
}
