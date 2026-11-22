//! Event correctness on the transactional in-memory reference. The fixture
//! claims real reference leases; production lifecycle wiring belongs to T12.

use std::sync::Arc;

use orch_core::store::{events::*, fake::FakeStorage, *};
use orch_domain::{RunEvent, RunEventType};

fn caller(project: &str) -> Caller {
    Caller {
        project: project.into(),
        principal: Principal::Owner {
            identity: "synthetic".into(),
        },
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

pub async fn memory_backend() -> Arc<dyn Storage> {
    let storage = FakeStorage::default();
    storage.restore_to_lineage("current").await;
    for (project, run, task, epoch) in [
        ("p", "run", "task", 2),
        ("other", "run", "task", 2),
        ("p", "another", "task2", 3),
    ] {
        storage
            .transaction(caller(project), Lane::Claims, move |unit| {
                let new = leases::NewLease {
                    task: task.into(),
                    agent: task.into(),
                    run: run.into(),
                };
                for expected in 1..=epoch {
                    let leases::ClaimOutcome::Granted(lease) = unit.claim_lease(&new, 0)? else {
                        panic!("fresh fixture claim is blocked");
                    };
                    assert_eq!(lease.epoch, expected);
                    if expected < epoch {
                        unit.release_lease(task, "current", expected)?;
                    }
                }
                Ok(())
            })
            .await
            .unwrap();
    }
    Arc::new(storage)
}

async fn append(
    storage: &dyn Storage,
    project: &str,
    batch: Vec<SequencedEvent>,
) -> Result<AppendResult> {
    storage
        .transaction(caller(project), Lane::Events, move |unit| {
            unit.append_events("run", "current", 2, &batch)
        })
        .await
}

pub async fn duplicate_counts_first_bytes_and_range_tail_order(storage: Arc<dyn Storage>) {
    assert_eq!(
        append(
            storage.as_ref(),
            "p",
            vec![event(3), event(1), event(2), event(2)]
        )
        .await
        .unwrap(),
        AppendResult {
            stored: 3,
            skipped: 1
        }
    );
    let mut replacement = event(2);
    replacement.event.data = serde_json::json!("replacement");
    assert_eq!(
        append(storage.as_ref(), "p", vec![replacement, event(4)])
            .await
            .unwrap(),
        AppendResult {
            stored: 1,
            skipped: 1
        }
    );
    let view = storage.read(caller("p")).await.unwrap();
    assert_eq!(
        view.read_events("run", 1, 2).await.unwrap(),
        vec![event(2), event(3)]
    );
    assert_eq!(
        view.read_events_tail("run", 2).await.unwrap(),
        vec![event(3), event(4)]
    );
    assert_eq!(
        view.read_events_tail("run", 100).await.unwrap(),
        (1..=4).map(event).collect::<Vec<_>>()
    );
    assert!(
        view.read_events("run", u64::MAX, 1)
            .await
            .unwrap()
            .is_empty()
    );
}

pub async fn projects_and_runs_are_separate_and_reads_are_snapshots(storage: Arc<dyn Storage>) {
    let before = storage.read(caller("p")).await.unwrap();
    append(storage.as_ref(), "p", vec![event(1)]).await.unwrap();
    assert!(before.read_events("run", 0, 10).await.unwrap().is_empty());
    let other = storage.read(caller("other")).await.unwrap();
    assert!(other.read_events("run", 0, 10).await.unwrap().is_empty());
    assert!(matches!(
        append(storage.as_ref(), "unseeded", vec![event(1)]).await,
        Err(StoreError::StaleEpoch)
    ));
    append(storage.as_ref(), "other", vec![event(8)])
        .await
        .unwrap();
    let view = storage.read(caller("p")).await.unwrap();
    assert_eq!(
        view.read_events("run", 0, 10).await.unwrap(),
        vec![event(1)]
    );
    assert!(
        view.read_events_tail("missing", 10)
            .await
            .unwrap()
            .is_empty()
    );
}

pub async fn stale_lineage_epoch_missing_lease_and_task_mismatch_change_nothing(
    storage: Arc<dyn Storage>,
) {
    append(storage.as_ref(), "p", vec![event(1)]).await.unwrap();
    for (run, lineage, epoch) in [
        ("run", "old", 2),
        ("run", "current", 1),
        ("missing", "current", 2),
    ] {
        for batch in [vec![], vec![event(1)], vec![event(2)]] {
            assert!(matches!(
                storage
                    .transaction(caller("p"), Lane::Events, move |u| u
                        .append_events(run, lineage, epoch, &batch))
                    .await,
                Err(StoreError::StaleEpoch)
            ));
        }
    }
    for (task, run, lineage, epoch) in [
        ("wrong", "run", "current", 2),
        ("task", "wrong", "current", 2),
        ("task", "run", "old", 2),
        ("task", "run", "current", 1),
    ] {
        let caller = Caller {
            project: "p".into(),
            principal: Principal::Task {
                identity: "synthetic".into(),
                task: task.into(),
                run: run.into(),
                lineage: lineage.into(),
                epoch,
            },
        };
        assert!(matches!(
            storage
                .transaction(caller, Lane::Events, |u| u.append_events(
                    "run",
                    "current",
                    2,
                    &[event(2)]
                ))
                .await,
            Err(StoreError::StaleEpoch)
        ));
    }
    assert_eq!(
        storage
            .read(caller("p"))
            .await
            .unwrap()
            .read_events("run", 0, 10)
            .await
            .unwrap(),
        vec![event(1)]
    );
}

pub async fn transaction_error_rolls_back_append_gap_prune_and_entities(storage: Arc<dyn Storage>) {
    append(storage.as_ref(), "p", vec![event(1), event(2)])
        .await
        .unwrap();
    let result: Result<()> = storage
        .transaction(caller("p"), Lane::Events, |u| {
            u.append_events("run", "current", 2, &[event(3)])?;
            u.record_event_gap("run", 3, 2)?;
            u.prune_events("run", 1)?;
            u.create(Entity::Task(Box::new(crate::contract::task(
                "tsk_rollback",
            ))))?;
            Err(StoreError::InvalidEvents("injected rollback".into()))
        })
        .await;
    assert!(result.is_err());
    let view = storage.read(caller("p")).await.unwrap();
    assert_eq!(
        view.read_events("run", 0, 10).await.unwrap(),
        vec![event(1), event(2)]
    );
    assert!(view.read_event_gaps("run").await.unwrap().is_empty());
    assert!(
        view.get(EntityKind::Task, "tsk_rollback")
            .await
            .unwrap()
            .is_none()
    );
}

pub async fn gaps_and_bounded_prune_preserve_other_runs_and_projects(storage: Arc<dyn Storage>) {
    append(storage.as_ref(), "other", vec![event(8)])
        .await
        .unwrap();
    append(storage.as_ref(), "p", vec![event(1), event(4), event(5)])
        .await
        .unwrap();
    storage
        .transaction(caller("p"), Lane::Events, |u| {
            u.record_event_gap("run", 1, 2)?;
            u.append_events("another", "current", 3, &[event(9)])?;
            Ok(())
        })
        .await
        .unwrap();
    let snapshot = storage.read(caller("p")).await.unwrap();
    for (deleted, complete) in [(2, false), (1, true), (0, true)] {
        assert_eq!(
            storage
                .transaction(caller("p"), Lane::Maintenance, |u| u.prune_events("run", 2))
                .await
                .unwrap(),
            PruneResult { deleted, complete }
        );
        if !complete {
            assert_eq!(
                storage
                    .read(caller("p"))
                    .await
                    .unwrap()
                    .read_events("run", 0, 10)
                    .await
                    .unwrap(),
                vec![event(5)]
            );
        }
    }
    let view = storage.read(caller("p")).await.unwrap();
    assert_eq!(
        view.read_events("another", 0, 10).await.unwrap(),
        vec![event(9)]
    );
    assert_eq!(
        view.read_event_gaps("run").await.unwrap(),
        vec![EventGap {
            after_seq: 1,
            dropped: 2
        }]
    );
    assert_eq!(snapshot.read_events("run", 0, 10).await.unwrap().len(), 3);
    assert_eq!(
        storage
            .read(caller("other"))
            .await
            .unwrap()
            .read_events("run", 0, 10)
            .await
            .unwrap(),
        vec![event(8)]
    );
    assert!(
        storage
            .read(caller("other"))
            .await
            .unwrap()
            .read_event_gaps("run")
            .await
            .unwrap()
            .is_empty()
    );
}

pub async fn empty_tails_batches_and_invalid_bounds_are_explicit(storage: Arc<dyn Storage>) {
    assert_eq!(
        append(storage.as_ref(), "p", vec![]).await.unwrap(),
        AppendResult {
            stored: 0,
            skipped: 0
        }
    );
    let view = storage.read(caller("p")).await.unwrap();
    assert!(view.read_events_tail("run", 0).await.unwrap().is_empty());
    assert!(view.read_events_tail("run", 10).await.unwrap().is_empty());
    for bound in [0, 1_001] {
        assert!(view.read_events("run", 0, bound).await.is_err());
        assert!(
            storage
                .transaction(caller("p"), Lane::Maintenance, move |u| u
                    .prune_events("run", bound))
                .await
                .is_err()
        );
    }
    assert!(view.read_events_tail("run", 1_001).await.is_err());
    storage
        .transaction(caller("p"), Lane::Events, |u| {
            assert!(
                u.append_events("run", "current", 2, &[event(1), event(0)])
                    .is_err()
            );
            assert!(u.record_event_gap("run", 1, 0).is_err());
            assert!(u.record_event_gap("run", u64::MAX, 1).is_err());
            Ok(())
        })
        .await
        .unwrap();
    let after = storage.read(caller("p")).await.unwrap();
    assert!(after.read_events("run", 0, 10).await.unwrap().is_empty());
    assert!(after.read_event_gaps("run").await.unwrap().is_empty());
}

pub async fn matching_task_fence_is_accepted(storage: Arc<dyn Storage>) {
    let authority = Caller {
        project: "p".into(),
        principal: Principal::Task {
            identity: "synthetic".into(),
            task: "task".into(),
            run: "run".into(),
            lineage: "current".into(),
            epoch: 2,
        },
    };
    storage
        .transaction(authority, Lane::Events, |u| {
            assert_eq!(
                u.append_events("run", "current", 2, &[event(1), event(2)])?,
                AppendResult {
                    stored: 2,
                    skipped: 0
                }
            );
            u.record_event_gap("run", 2, 1)?;
            Ok(())
        })
        .await
        .unwrap();
    let view = storage.read(caller("p")).await.unwrap();
    assert!(view.read_events_tail("run", 0).await.unwrap().is_empty());
    assert_eq!(
        view.read_events("run", 1, 1_000).await.unwrap(),
        vec![event(2)]
    );
    assert_eq!(
        view.read_event_gaps("run").await.unwrap(),
        vec![EventGap {
            after_seq: 2,
            dropped: 1
        }]
    );
}

pub async fn repeated_gap_is_idempotent_and_conflict_changes_nothing(storage: Arc<dyn Storage>) {
    for attempt in 0..3 {
        storage
            .transaction(caller("p"), Lane::Events, move |u| {
                if attempt < 2 {
                    u.record_event_gap("run", 5, 3)?;
                } else {
                    // Catch the refusal and commit: operation atomicity must still hold.
                    assert!(matches!(
                        u.record_event_gap("run", 5, 4),
                        Err(StoreError::InvalidEvents(_))
                    ));
                }
                Ok(())
            })
            .await
            .unwrap();
        assert_eq!(
            storage
                .read(caller("p"))
                .await
                .unwrap()
                .read_event_gaps("run")
                .await
                .unwrap(),
            vec![EventGap {
                after_seq: 5,
                dropped: 3
            }]
        );
    }
    storage
        .transaction(caller("other"), Lane::Events, |u| {
            u.record_event_gap("run", 5, 9)
        })
        .await
        .unwrap();
    storage
        .transaction(caller("p"), Lane::Events, |u| {
            u.record_event_gap("another", 5, 8)
        })
        .await
        .unwrap();
    assert_eq!(
        storage
            .read(caller("p"))
            .await
            .unwrap()
            .read_event_gaps("run")
            .await
            .unwrap(),
        vec![EventGap {
            after_seq: 5,
            dropped: 3
        }]
    );
}

macro_rules! event_suite {
    ($module:ident, $factory:path) => {
        mod $module {
            macro_rules! case {
                ($name:ident) => {
                    #[tokio::test]
                    async fn $name() {
                        crate::events_v2::$name($factory().await).await;
                    }
                };
            }
            case!(duplicate_counts_first_bytes_and_range_tail_order);
            case!(projects_and_runs_are_separate_and_reads_are_snapshots);
            case!(stale_lineage_epoch_missing_lease_and_task_mismatch_change_nothing);
            case!(transaction_error_rolls_back_append_gap_prune_and_entities);
            case!(gaps_and_bounded_prune_preserve_other_runs_and_projects);
            case!(empty_tails_batches_and_invalid_bounds_are_explicit);
            case!(matching_task_fence_is_accepted);
            case!(repeated_gap_is_idempotent_and_conflict_changes_nothing);
        }
    };
}
pub(crate) use event_suite;
