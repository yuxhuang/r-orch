//! Backend-neutral lease scenarios, including commit and rollback.
#![allow(
    clippy::unwrap_used,
    reason = "helpers outside #[test] functions; clippy.toml allows unwrap only inside them"
)]

use crate::v2_operations::Fixture;
use orch_core::store::{leases::*, *};

const P: &str = "proj";

fn owner(project: &str) -> Caller {
    Caller {
        project: project.into(),
        principal: Principal::Owner {
            identity: "owner".into(),
        },
    }
}

fn worker(project: &str) -> Caller {
    Caller {
        project: project.into(),
        principal: Principal::Worker {
            identity: "w".into(),
        },
    }
}

fn holder(project: &str, l: &Lease) -> Caller {
    Caller {
        project: project.into(),
        principal: Principal::Task {
            identity: "t".into(),
            task: l.task.clone(),
            run: l.run.clone(),
            lineage: l.lineage.clone(),
            epoch: l.epoch,
        },
    }
}

fn new(task: &str, agent: &str, run: &str) -> NewLease {
    NewLease {
        task: task.into(),
        agent: agent.into(),
        run: run.into(),
    }
}

async fn claim(s: &dyn Storage, project: &str, n: NewLease, now: i64) -> ClaimOutcome {
    s.transaction(owner(project), Lane::Claims, move |u| {
        u.claim_lease(&n, now)
    })
    .await
    .unwrap()
}

fn granted(o: ClaimOutcome) -> Lease {
    match o {
        ClaimOutcome::Granted(l) => l,
        ClaimOutcome::Held(l) => panic!("held by {l:?}"),
    }
}

fn held(o: ClaimOutcome) -> Lease {
    match o {
        ClaimOutcome::Held(l) => l,
        ClaimOutcome::Granted(l) => panic!("granted {l:?}"),
    }
}

async fn renew(s: &dyn Storage, who: Caller, l: &Lease, now: i64) -> Result<Lease> {
    let (task, lineage, epoch) = (l.task.clone(), l.lineage.clone(), l.epoch);
    s.transaction(who, Lane::Claims, move |u| {
        u.renew_lease(&task, &lineage, epoch, now)
    })
    .await
}

async fn release(s: &dyn Storage, who: Caller, l: &Lease) -> Result<Lease> {
    let (task, lineage, epoch) = (l.task.clone(), l.lineage.clone(), l.epoch);
    s.transaction(who, Lane::Claims, move |u| {
        u.release_lease(&task, &lineage, epoch)
    })
    .await
}

async fn all(s: &dyn Storage, project: &str) -> Vec<Lease> {
    s.read(owner(project))
        .await
        .unwrap()
        .read_leases()
        .await
        .unwrap()
}

pub async fn a_claim_grants_the_next_epoch_under_the_current_lineage(fixture: Fixture) {
    let s = fixture.storage.clone();
    let a = granted(claim(s.as_ref(), P, new("t1", "a1", "r1"), 1_000).await);
    assert_eq!(a.epoch, 1);
    assert_eq!(a.lineage, fixture.initial_lineage);
    assert_eq!(a.expires_at, 121_000);
    assert_eq!(all(s.as_ref(), P).await, vec![a.clone()]);
    let snap = s.read(owner(P)).await.unwrap();
    assert_eq!(snap.read_lease("t1").await.unwrap(), Some(a.clone()));
    assert_eq!(snap.read_agent_lease("a1").await.unwrap(), Some(a));
    assert_eq!(snap.read_lease("t2").await.unwrap(), None);
}

pub async fn an_agent_holds_at_most_one_active_lease(fixture: Fixture) {
    let s = fixture.storage.clone();
    let first = granted(claim(s.as_ref(), P, new("t1", "a1", "r1"), 0).await);
    // Same task, other agent; same agent, other task; and the run id in use.
    assert_eq!(
        held(claim(s.as_ref(), P, new("t1", "a2", "r2"), 0).await),
        first
    );
    assert_eq!(
        held(claim(s.as_ref(), P, new("t2", "a1", "r2"), 0).await),
        first
    );
    assert_eq!(
        held(claim(s.as_ref(), P, new("t2", "a2", "r1"), 0).await),
        first
    );
    assert_eq!(all(s.as_ref(), P).await, vec![first]);
}

pub async fn the_task_lease_is_named_before_the_agent_lease(fixture: Fixture) {
    let s = fixture.storage.clone();
    let on_t1 = granted(claim(s.as_ref(), P, new("t1", "a1", "r1"), 0).await);
    let on_t2 = granted(claim(s.as_ref(), P, new("t2", "a2", "r2"), 0).await);
    assert_eq!(
        held(claim(s.as_ref(), P, new("t1", "a2", "r3"), 0).await),
        on_t1
    );
    assert_eq!(
        held(claim(s.as_ref(), P, new("t2", "a1", "r3"), 0).await),
        on_t2
    );
}

pub async fn a_refused_claim_uses_no_epoch(fixture: Fixture) {
    let s = fixture.storage.clone();
    let first = granted(claim(s.as_ref(), P, new("t1", "a1", "r1"), 0).await);
    for i in 0..3 {
        held(
            claim(
                s.as_ref(),
                P,
                new("t1", &format!("b{i}"), &format!("x{i}")),
                0,
            )
            .await,
        );
    }
    release(s.as_ref(), owner(P), &first).await.unwrap();
    assert_eq!(
        granted(claim(s.as_ref(), P, new("t1", "a2", "r2"), 0).await).epoch,
        2
    );
}

pub async fn one_of_many_concurrent_claims_wins(fixture: Fixture) {
    let s = fixture.storage.clone();
    // One task, three agents; then three tasks, one agent. No FIFO promise.
    let (x, y, z) = tokio::join!(
        claim(s.as_ref(), P, new("t1", "a1", "r1"), 0),
        claim(s.as_ref(), P, new("t1", "a2", "r2"), 0),
        claim(s.as_ref(), P, new("t1", "a3", "r3"), 0),
    );
    assert_one_winner([x, y, z]);

    let (x, y, z) = tokio::join!(
        claim(s.as_ref(), P, new("u1", "b1", "q1"), 0),
        claim(s.as_ref(), P, new("u2", "b1", "q2"), 0),
        claim(s.as_ref(), P, new("u3", "b1", "q3"), 0),
    );
    assert_one_winner([x, y, z]);
    assert_eq!(all(s.as_ref(), P).await.len(), 2);
}

pub async fn a_claim_rolls_back_with_its_unit_of_work(fixture: Fixture) {
    let s = fixture.storage.clone();
    let failed = s
        .transaction(owner(P), Lane::Claims, |u| {
            let lease = granted(u.claim_lease(&new("t1", "a1", "r1"), 0)?);
            assert_eq!(lease.epoch, 1);
            u.get_lease("t1")?
                .expect("visible inside its own transaction");
            Err::<(), _>(StoreError::StaleEpoch)
        })
        .await;
    assert!(matches!(failed, Err(StoreError::StaleEpoch)));
    assert!(all(s.as_ref(), P).await.is_empty());
    // The epoch went back with it: the next claim is the first.
    assert_eq!(
        granted(claim(s.as_ref(), P, new("t1", "a1", "r1"), 0).await).epoch,
        1
    );
}

pub async fn agent_lookup_sees_own_claim_and_keeps_only_committed_leases_after_rollback(
    fixture: Fixture,
) {
    let s = fixture.storage.clone();
    let committed = granted(claim(s.as_ref(), P, new("t1", "a1", "r1"), 10).await);
    let expected = committed.clone();
    let failed = s
        .transaction(owner(P), Lane::Claims, move |u| {
            let pending = granted(u.claim_lease(&new("t2", "a2", "r2"), 20)?);
            assert_eq!(u.get_agent_lease("a2")?, Some(pending));
            assert_eq!(u.get_agent_lease("a1")?, Some(expected));
            assert_eq!(u.get_agent_lease("t2")?, None, "lookup uses the agent id");
            Err::<(), _>(StoreError::StaleEpoch)
        })
        .await;
    assert!(matches!(failed, Err(StoreError::StaleEpoch)));
    let snap = s.read(owner(P)).await.unwrap();
    assert_eq!(snap.read_agent_lease("a2").await.unwrap(), None);
    assert_eq!(
        snap.read_agent_lease("a1").await.unwrap(),
        Some(committed.clone())
    );
    s.transaction(owner(P), Lane::Claims, move |u| {
        assert_eq!(u.get_agent_lease("a2")?, None);
        assert_eq!(u.get_agent_lease("a1")?, Some(committed));
        Ok(())
    })
    .await
    .unwrap();
}

pub async fn lease_list_sees_every_own_claim_and_keeps_only_committed_leases_after_rollback(
    fixture: Fixture,
) {
    let s = fixture.storage.clone();
    let committed = granted(claim(s.as_ref(), P, new("t1", "a1", "r1"), 10).await);
    // A project-scoped list must not return a neighboring project's lease.
    claim(s.as_ref(), "other", new("t0", "a0", "r0"), 10).await;
    let expected = committed.clone();
    let failed = s
        .transaction(owner(P), Lane::Claims, move |u| {
            let second = granted(u.claim_lease(&new("t2", "a2", "r2"), 20)?);
            let third = granted(u.claim_lease(&new("t3", "a3", "r3"), 30)?);
            assert_eq!(u.list_leases()?, vec![expected, second, third]);
            Err::<(), _>(StoreError::StaleEpoch)
        })
        .await;
    assert!(matches!(failed, Err(StoreError::StaleEpoch)));
    assert_eq!(all(s.as_ref(), P).await, vec![committed.clone()]);
    s.transaction(owner(P), Lane::Claims, move |u| {
        assert_eq!(u.list_leases()?, vec![committed]);
        Ok(())
    })
    .await
    .unwrap();
}

pub async fn release_and_renew_roll_back_with_their_unit_of_work(fixture: Fixture) {
    let s = fixture.storage.clone();
    let l = granted(claim(s.as_ref(), P, new("t1", "a1", "r1"), 0).await);
    let (task, lineage) = (l.task.clone(), l.lineage.clone());
    let failed = s
        .transaction(owner(P), Lane::Claims, move |u| {
            u.renew_lease(&task, &lineage, 1, 50)?;
            u.release_lease(&task, &lineage, 1)?;
            assert!(u.get_lease(&task)?.is_none());
            Err::<(), _>(StoreError::StaleEpoch)
        })
        .await;
    assert!(failed.is_err());
    assert_eq!(all(s.as_ref(), P).await, vec![l]);
}

pub async fn renew_moves_the_deadline_from_the_callers_clock(fixture: Fixture) {
    let s = fixture.storage.clone();
    let l = granted(claim(s.as_ref(), P, new("t1", "a1", "r1"), 10).await);
    let renewed = renew(s.as_ref(), owner(P), &l, 40).await.unwrap();
    assert_eq!(
        renewed,
        Lease {
            expires_at: 120_040,
            ..l.clone()
        }
    );
    assert_eq!(all(s.as_ref(), P).await, vec![renewed]);
}

pub async fn a_lease_past_its_deadline_is_renewable_until_someone_ends_it(fixture: Fixture) {
    // AP12 / R42: after a suspend the daemon renews what it supervises before
    // it expires anything, so neither renew nor a rival claim reads the deadline.
    let s = fixture.storage.clone();
    let l = granted(claim(s.as_ref(), P, new("t1", "a1", "r1"), 0).await);
    let late = l.expires_at + 10_000;
    assert_eq!(
        held(claim(s.as_ref(), P, new("t1", "a2", "r2"), late).await),
        l
    );
    assert_eq!(
        renew(s.as_ref(), owner(P), &l, late)
            .await
            .unwrap()
            .expires_at,
        late + LEASE_MS
    );
}

pub async fn release_ends_the_lease_and_keeps_the_epoch_counter(fixture: Fixture) {
    let s = fixture.storage.clone();
    let first = granted(claim(s.as_ref(), P, new("t1", "a1", "r1"), 0).await);
    assert_eq!(release(s.as_ref(), owner(P), &first).await.unwrap(), first);
    assert!(all(s.as_ref(), P).await.is_empty());
    let second = granted(claim(s.as_ref(), P, new("t1", "a1", "r2"), 0).await);
    assert_eq!(second.epoch, 2);
    // The released token never matches the new lease, whoever presents it.
    for who in [owner(P), worker(P), holder(P, &first)] {
        assert!(matches!(
            renew(s.as_ref(), who.clone(), &first, 1).await,
            Err(StoreError::StaleEpoch)
        ));
        assert!(matches!(
            release(s.as_ref(), who, &first).await,
            Err(StoreError::StaleEpoch)
        ));
    }
    assert_eq!(all(s.as_ref(), P).await, vec![second]);
}

pub async fn a_second_release_is_stale(fixture: Fixture) {
    let s = fixture.storage.clone();
    let l = granted(claim(s.as_ref(), P, new("t1", "a1", "r1"), 0).await);
    release(s.as_ref(), owner(P), &l).await.unwrap();
    assert!(matches!(
        release(s.as_ref(), owner(P), &l).await,
        Err(StoreError::StaleEpoch)
    ));
}

pub async fn a_stale_epoch_is_refused(fixture: Fixture) {
    let s = fixture.storage.clone();
    let l = granted(claim(s.as_ref(), P, new("t1", "a1", "r1"), 0).await);
    let before = all(s.as_ref(), P).await;
    for bad in [
        Lease {
            epoch: l.epoch + 1,
            ..l.clone()
        },
        Lease {
            epoch: 0,
            ..l.clone()
        },
        Lease {
            lineage: "other".into(),
            ..l.clone()
        },
        Lease {
            task: "t2".into(),
            ..l.clone()
        },
    ] {
        assert!(matches!(
            renew(s.as_ref(), owner(P), &bad, 99).await,
            Err(StoreError::StaleEpoch)
        ));
        assert!(matches!(
            release(s.as_ref(), owner(P), &bad).await,
            Err(StoreError::StaleEpoch)
        ));
        assert_eq!(all(s.as_ref(), P).await, before);
    }
}

pub async fn a_restore_fences_every_earlier_lineage_and_reissues_no_pair(fixture: Fixture) {
    let s = fixture.storage.clone();
    let old = granted(claim(s.as_ref(), P, new("t1", "a1", "r1"), 0).await);
    fixture.restore.restore_to_lineage("next-lineage").await;
    // R138 ended the lease; the old holder is refused, and still after a new claim.
    assert!(all(s.as_ref(), P).await.is_empty());
    assert!(matches!(
        renew(s.as_ref(), holder(P, &old), &old, 1).await,
        Err(StoreError::StaleEpoch)
    ));
    let new_lease = granted(claim(s.as_ref(), P, new("t1", "a1", "r2"), 0).await);
    assert_eq!(new_lease.lineage, "next-lineage");
    assert_eq!(new_lease.epoch, 2, "the counter survives the restore");
    assert_ne!(
        (&old.lineage, old.epoch),
        (&new_lease.lineage, new_lease.epoch)
    );
    for who in [owner(P), worker(P), holder(P, &old)] {
        assert!(matches!(
            renew(s.as_ref(), who.clone(), &old, 5).await,
            Err(StoreError::StaleEpoch)
        ));
        assert!(matches!(
            release(s.as_ref(), who, &old).await,
            Err(StoreError::StaleEpoch)
        ));
    }
    assert_eq!(all(s.as_ref(), P).await, vec![new_lease.clone()]);
    // A caller that presents the new epoch under the old lineage is refused too.
    let forged = Lease {
        lineage: old.lineage.clone(),
        ..new_lease
    };
    assert!(matches!(
        renew(s.as_ref(), owner(P), &forged, 5).await,
        Err(StoreError::StaleEpoch)
    ));
}

pub async fn a_task_principal_may_touch_only_its_own_lease(fixture: Fixture) {
    let s = fixture.storage.clone();
    let mine = granted(claim(s.as_ref(), P, new("t1", "a1", "r1"), 0).await);
    let other = granted(claim(s.as_ref(), P, new("t2", "a2", "r2"), 0).await);
    let before = all(s.as_ref(), P).await;
    // Holder of t2 names t1's current lineage and epoch: refused, nothing changed.
    assert!(matches!(
        renew(s.as_ref(), holder(P, &other), &mine, 7).await,
        Err(StoreError::StaleEpoch)
    ));
    assert!(matches!(
        release(s.as_ref(), holder(P, &other), &mine).await,
        Err(StoreError::StaleEpoch)
    ));
    // A principal with the right task but another run, epoch or lineage.
    for who in [
        Principal::Task {
            identity: "t".into(),
            task: "t1".into(),
            run: "rX".into(),
            lineage: mine.lineage.clone(),
            epoch: 1,
        },
        Principal::Task {
            identity: "t".into(),
            task: "t1".into(),
            run: "r1".into(),
            lineage: mine.lineage.clone(),
            epoch: 2,
        },
        Principal::Task {
            identity: "t".into(),
            task: "t1".into(),
            run: "r1".into(),
            lineage: "other".into(),
            epoch: 1,
        },
    ] {
        let caller = Caller {
            project: P.into(),
            principal: who,
        };
        assert!(matches!(
            renew(s.as_ref(), caller.clone(), &mine, 7).await,
            Err(StoreError::StaleEpoch)
        ));
        assert!(matches!(
            release(s.as_ref(), caller, &mine).await,
            Err(StoreError::StaleEpoch)
        ));
    }
    assert_eq!(all(s.as_ref(), P).await, before);
    // The holder itself may renew and release.
    assert_eq!(
        renew(s.as_ref(), holder(P, &mine), &mine, 7)
            .await
            .unwrap()
            .expires_at,
        7 + LEASE_MS
    );
    release(s.as_ref(), holder(P, &mine), &mine).await.unwrap();
}

pub async fn another_project_sees_no_lease_and_changes_none(fixture: Fixture) {
    let s = fixture.storage.clone();
    let l = granted(claim(s.as_ref(), "p1", new("t1", "a1", "r1"), 0).await);
    for who in [owner("p2"), worker("p2"), holder("p2", &l)] {
        assert!(matches!(
            renew(s.as_ref(), who.clone(), &l, 5).await,
            Err(StoreError::StaleEpoch)
        ));
        assert!(matches!(
            release(s.as_ref(), who, &l).await,
            Err(StoreError::StaleEpoch)
        ));
    }
    assert_eq!(all(s.as_ref(), "p1").await, vec![l.clone()]);
    assert!(all(s.as_ref(), "p2").await.is_empty());
    // The same names in the other project are independent: own agent slot, own epoch.
    let twin = granted(claim(s.as_ref(), "p2", new("t1", "a1", "r1"), 0).await);
    assert_eq!(twin.epoch, 1);
    assert_eq!(all(s.as_ref(), "p1").await, vec![l]);
}

pub async fn epochs_only_grow(fixture: Fixture) {
    let s = fixture.storage.clone();
    let mut seen = Vec::new();
    for i in 0..4 {
        let l = granted(claim(s.as_ref(), P, new("t1", "a1", &format!("r{i}")), 0).await);
        seen.push(l.epoch);
        release(s.as_ref(), owner(P), &l).await.unwrap();
    }
    assert_eq!(seen, [1, 2, 3, 4]);
}

pub async fn a_clock_that_would_overflow_is_refused_without_a_change(fixture: Fixture) {
    let s = fixture.storage.clone();
    let n = new("t1", "a1", "r1");
    let r = s
        .transaction(owner(P), Lane::Claims, move |u| u.claim_lease(&n, i64::MAX))
        .await;
    assert!(matches!(r, Err(StoreError::CounterExhausted)));
    assert!(all(s.as_ref(), P).await.is_empty());
    let l = granted(claim(s.as_ref(), P, new("t1", "a1", "r1"), 0).await);
    assert_eq!(l.epoch, 1, "the refused claim used no epoch");
    assert!(matches!(
        renew(s.as_ref(), owner(P), &l, i64::MAX).await,
        Err(StoreError::CounterExhausted)
    ));
    assert_eq!(all(s.as_ref(), P).await, vec![l]);
}

pub async fn a_task_principal_that_differs_only_in_its_task_is_refused(fixture: Fixture) {
    let s = fixture.storage.clone();
    let l = granted(claim(s.as_ref(), P, new("t1", "a1", "r1"), 0).await);
    let caller = Caller {
        project: P.into(),
        principal: Principal::Task {
            identity: "t".into(),
            task: "t2".into(),
            run: l.run.clone(),
            lineage: l.lineage.clone(),
            epoch: l.epoch,
        },
    };
    assert!(matches!(
        renew(s.as_ref(), caller.clone(), &l, 7).await,
        Err(StoreError::StaleEpoch)
    ));
    assert!(matches!(
        release(s.as_ref(), caller, &l).await,
        Err(StoreError::StaleEpoch)
    ));
    assert_eq!(all(s.as_ref(), P).await, vec![l]);
}

fn assert_one_winner(outcomes: [ClaimOutcome; 3]) {
    let winners: Vec<_> = outcomes
        .iter()
        .filter_map(|o| match o {
            ClaimOutcome::Granted(lease) => Some(lease),
            ClaimOutcome::Held(_) => None,
        })
        .collect();
    assert_eq!(winners.len(), 1);
    for outcome in &outcomes {
        if let ClaimOutcome::Held(lease) = outcome {
            assert_eq!(lease, winners[0]);
        }
    }
}

macro_rules! lease_suite {
    ($module:ident, $factory:path) => {
        mod $module {
            macro_rules! case {
                ($name:ident) => {
                    #[tokio::test]
                    async fn $name() {
                        crate::leases_v2::$name($factory().await).await;
                    }
                };
            }
            case!(a_claim_grants_the_next_epoch_under_the_current_lineage);
            case!(an_agent_holds_at_most_one_active_lease);
            case!(the_task_lease_is_named_before_the_agent_lease);
            case!(a_refused_claim_uses_no_epoch);
            case!(one_of_many_concurrent_claims_wins);
            case!(a_claim_rolls_back_with_its_unit_of_work);
            case!(agent_lookup_sees_own_claim_and_keeps_only_committed_leases_after_rollback);
            case!(lease_list_sees_every_own_claim_and_keeps_only_committed_leases_after_rollback);
            case!(release_and_renew_roll_back_with_their_unit_of_work);
            case!(renew_moves_the_deadline_from_the_callers_clock);
            case!(a_lease_past_its_deadline_is_renewable_until_someone_ends_it);
            case!(release_ends_the_lease_and_keeps_the_epoch_counter);
            case!(a_second_release_is_stale);
            case!(a_stale_epoch_is_refused);
            case!(a_restore_fences_every_earlier_lineage_and_reissues_no_pair);
            case!(a_task_principal_may_touch_only_its_own_lease);
            case!(another_project_sees_no_lease_and_changes_none);
            case!(epochs_only_grow);
            case!(a_clock_that_would_overflow_is_refused_without_a_change);
            case!(a_task_principal_that_differs_only_in_its_task_is_refused);
        }
    };
}
pub(crate) use lease_suite;
