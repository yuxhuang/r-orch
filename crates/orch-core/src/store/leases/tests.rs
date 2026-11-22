//! Lease rules through the in-memory storage, so each one also shows that it
//! shares the unit of work's commit and rollback.
#![allow(
    clippy::unwrap_used,
    reason = "helpers outside #[test] functions; clippy.toml allows unwrap only inside them"
)]

use std::sync::Arc;

use super::*;
use crate::store::fake::FakeStorage;
use crate::store::{Caller, Lane, Principal, Storage, StorageExt, StoreError};

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

async fn claim(s: &FakeStorage, project: &str, n: NewLease, now: i64) -> ClaimOutcome {
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

async fn renew(s: &FakeStorage, who: Caller, l: &Lease, now: i64) -> crate::store::Result<Lease> {
    let (task, lineage, epoch) = (l.task.clone(), l.lineage.clone(), l.epoch);
    s.transaction(who, Lane::Claims, move |u| {
        u.renew_lease(&task, &lineage, epoch, now)
    })
    .await
}

async fn release(s: &FakeStorage, who: Caller, l: &Lease) -> crate::store::Result<Lease> {
    let (task, lineage, epoch) = (l.task.clone(), l.lineage.clone(), l.epoch);
    s.transaction(who, Lane::Claims, move |u| {
        u.release_lease(&task, &lineage, epoch)
    })
    .await
}

async fn all(s: &FakeStorage, project: &str) -> Vec<Lease> {
    s.read(owner(project))
        .await
        .unwrap()
        .read_leases()
        .await
        .unwrap()
}

#[tokio::test]
async fn a_claim_grants_the_next_epoch_under_the_current_lineage() {
    let s = FakeStorage::default();
    let a = granted(claim(&s, P, new("t1", "a1", "r1"), 1_000).await);
    assert_eq!(a.epoch, 1);
    assert_eq!(a.lineage, "fake-lineage-1");
    assert_eq!(a.expires_at, 1_000 + LEASE_SECS);
    assert_eq!(all(&s, P).await, vec![a.clone()]);
    let snap = s.read(owner(P)).await.unwrap();
    assert_eq!(snap.read_lease("t1").await.unwrap(), Some(a.clone()));
    assert_eq!(snap.read_agent_lease("a1").await.unwrap(), Some(a));
    assert_eq!(snap.read_lease("t2").await.unwrap(), None);
}

#[tokio::test]
async fn a_task_holds_one_lease_and_an_agent_holds_one() {
    let s = FakeStorage::default();
    let first = granted(claim(&s, P, new("t1", "a1", "r1"), 0).await);
    // Same task, other agent; same agent, other task; and the run id in use.
    assert_eq!(held(claim(&s, P, new("t1", "a2", "r2"), 0).await), first);
    assert_eq!(held(claim(&s, P, new("t2", "a1", "r2"), 0).await), first);
    assert_eq!(held(claim(&s, P, new("t2", "a2", "r1"), 0).await), first);
    assert_eq!(all(&s, P).await, vec![first]);
}

#[tokio::test]
async fn the_task_lease_is_named_before_the_agent_lease() {
    let s = FakeStorage::default();
    let on_t1 = granted(claim(&s, P, new("t1", "a1", "r1"), 0).await);
    let on_t2 = granted(claim(&s, P, new("t2", "a2", "r2"), 0).await);
    assert_eq!(held(claim(&s, P, new("t1", "a2", "r3"), 0).await), on_t1);
    assert_eq!(held(claim(&s, P, new("t2", "a1", "r3"), 0).await), on_t2);
}

#[tokio::test]
async fn a_refused_claim_uses_no_epoch() {
    let s = FakeStorage::default();
    let first = granted(claim(&s, P, new("t1", "a1", "r1"), 0).await);
    for i in 0..3 {
        held(claim(&s, P, new("t1", &format!("b{i}"), &format!("x{i}")), 0).await);
    }
    release(&s, owner(P), &first).await.unwrap();
    assert_eq!(
        granted(claim(&s, P, new("t1", "a2", "r2"), 0).await).epoch,
        2
    );
}

#[tokio::test]
async fn competing_claims_resolve_in_submission_order() {
    let s = Arc::new(FakeStorage::default());
    // One task, three agents; then three tasks, one agent. The first submitted
    // wins every time, on every run of this test.
    let (x, y, z) = tokio::join!(
        claim(&s, P, new("t1", "a1", "r1"), 0),
        claim(&s, P, new("t1", "a2", "r2"), 0),
        claim(&s, P, new("t1", "a3", "r3"), 0),
    );
    let winner = granted(x);
    assert_eq!(winner.agent, "a1");
    assert_eq!(held(y), winner);
    assert_eq!(held(z), winner);

    let (x, y, z) = tokio::join!(
        claim(&s, P, new("u1", "b1", "q1"), 0),
        claim(&s, P, new("u2", "b1", "q2"), 0),
        claim(&s, P, new("u3", "b1", "q3"), 0),
    );
    let winner = granted(x);
    assert_eq!(winner.task, "u1");
    assert_eq!(held(y), winner);
    assert_eq!(held(z), winner);
    assert_eq!(all(&s, P).await.len(), 2);
}

#[tokio::test]
async fn a_claim_rolls_back_with_its_unit_of_work() {
    let s = FakeStorage::default();
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
    assert!(all(&s, P).await.is_empty());
    // The epoch went back with it: the next claim is the first.
    assert_eq!(
        granted(claim(&s, P, new("t1", "a1", "r1"), 0).await).epoch,
        1
    );
}

#[tokio::test]
async fn release_and_renew_roll_back_with_their_unit_of_work() {
    let s = FakeStorage::default();
    let l = granted(claim(&s, P, new("t1", "a1", "r1"), 0).await);
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
    assert_eq!(all(&s, P).await, vec![l]);
}

#[tokio::test]
async fn renew_moves_the_deadline_from_the_callers_clock() {
    let s = FakeStorage::default();
    let l = granted(claim(&s, P, new("t1", "a1", "r1"), 10).await);
    let renewed = renew(&s, owner(P), &l, 40).await.unwrap();
    assert_eq!(
        renewed,
        Lease {
            expires_at: 40 + LEASE_SECS,
            ..l.clone()
        }
    );
    assert_eq!(all(&s, P).await, vec![renewed]);
}

#[tokio::test]
async fn a_lease_past_its_deadline_is_renewable_until_someone_ends_it() {
    // AP12 / R42: after a suspend the daemon renews what it supervises before
    // it expires anything, so neither renew nor a rival claim reads the deadline.
    let s = FakeStorage::default();
    let l = granted(claim(&s, P, new("t1", "a1", "r1"), 0).await);
    let late = l.expires_at + 10_000;
    assert_eq!(held(claim(&s, P, new("t1", "a2", "r2"), late).await), l);
    assert_eq!(
        renew(&s, owner(P), &l, late).await.unwrap().expires_at,
        late + LEASE_SECS
    );
}

#[tokio::test]
async fn release_ends_the_lease_and_keeps_the_epoch_counter() {
    let s = FakeStorage::default();
    let first = granted(claim(&s, P, new("t1", "a1", "r1"), 0).await);
    assert_eq!(release(&s, owner(P), &first).await.unwrap(), first);
    assert!(all(&s, P).await.is_empty());
    let second = granted(claim(&s, P, new("t1", "a1", "r2"), 0).await);
    assert_eq!(second.epoch, 2);
    // The released token never matches the new lease, whoever presents it.
    for who in [owner(P), worker(P), holder(P, &first)] {
        assert!(matches!(
            renew(&s, who.clone(), &first, 1).await,
            Err(StoreError::StaleEpoch)
        ));
        assert!(matches!(
            release(&s, who, &first).await,
            Err(StoreError::StaleEpoch)
        ));
    }
    assert_eq!(all(&s, P).await, vec![second]);
}

#[tokio::test]
async fn a_second_release_is_stale() {
    let s = FakeStorage::default();
    let l = granted(claim(&s, P, new("t1", "a1", "r1"), 0).await);
    release(&s, owner(P), &l).await.unwrap();
    assert!(matches!(
        release(&s, owner(P), &l).await,
        Err(StoreError::StaleEpoch)
    ));
}

#[tokio::test]
async fn a_wrong_epoch_or_lineage_changes_nothing() {
    let s = FakeStorage::default();
    let l = granted(claim(&s, P, new("t1", "a1", "r1"), 0).await);
    let before = all(&s, P).await;
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
            renew(&s, owner(P), &bad, 99).await,
            Err(StoreError::StaleEpoch)
        ));
        assert!(matches!(
            release(&s, owner(P), &bad).await,
            Err(StoreError::StaleEpoch)
        ));
        assert_eq!(all(&s, P).await, before);
    }
}

#[tokio::test]
async fn a_restore_fences_every_earlier_lineage_and_reissues_no_pair() {
    let s = FakeStorage::default();
    let old = granted(claim(&s, P, new("t1", "a1", "r1"), 0).await);
    s.restore_to_lineage("fake-lineage-2").await;
    // R138 ended the lease; the old holder is refused, and still after a new claim.
    assert!(all(&s, P).await.is_empty());
    assert!(matches!(
        renew(&s, holder(P, &old), &old, 1).await,
        Err(StoreError::StaleEpoch)
    ));
    let new_lease = granted(claim(&s, P, new("t1", "a1", "r2"), 0).await);
    assert_eq!(new_lease.lineage, "fake-lineage-2");
    assert_eq!(new_lease.epoch, 2, "the counter survives the restore");
    assert_ne!(
        (&old.lineage, old.epoch),
        (&new_lease.lineage, new_lease.epoch)
    );
    for who in [owner(P), worker(P), holder(P, &old)] {
        assert!(matches!(
            renew(&s, who.clone(), &old, 5).await,
            Err(StoreError::StaleEpoch)
        ));
        assert!(matches!(
            release(&s, who, &old).await,
            Err(StoreError::StaleEpoch)
        ));
    }
    assert_eq!(all(&s, P).await, vec![new_lease.clone()]);
    // A caller that presents the new epoch under the old lineage is refused too.
    let forged = Lease {
        lineage: old.lineage.clone(),
        ..new_lease
    };
    assert!(matches!(
        renew(&s, owner(P), &forged, 5).await,
        Err(StoreError::StaleEpoch)
    ));
}

#[tokio::test]
async fn a_task_principal_may_touch_only_its_own_lease() {
    let s = FakeStorage::default();
    let mine = granted(claim(&s, P, new("t1", "a1", "r1"), 0).await);
    let other = granted(claim(&s, P, new("t2", "a2", "r2"), 0).await);
    let before = all(&s, P).await;
    // Holder of t2 names t1's current lineage and epoch: refused, nothing changed.
    assert!(matches!(
        renew(&s, holder(P, &other), &mine, 7).await,
        Err(StoreError::StaleEpoch)
    ));
    assert!(matches!(
        release(&s, holder(P, &other), &mine).await,
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
            renew(&s, caller.clone(), &mine, 7).await,
            Err(StoreError::StaleEpoch)
        ));
        assert!(matches!(
            release(&s, caller, &mine).await,
            Err(StoreError::StaleEpoch)
        ));
    }
    assert_eq!(all(&s, P).await, before);
    // The holder itself may renew and release.
    assert_eq!(
        renew(&s, holder(P, &mine), &mine, 7)
            .await
            .unwrap()
            .expires_at,
        7 + LEASE_SECS
    );
    release(&s, holder(P, &mine), &mine).await.unwrap();
}

#[tokio::test]
async fn another_project_sees_no_lease_and_changes_none() {
    let s = FakeStorage::default();
    let l = granted(claim(&s, "p1", new("t1", "a1", "r1"), 0).await);
    for who in [owner("p2"), worker("p2"), holder("p2", &l)] {
        assert!(matches!(
            renew(&s, who.clone(), &l, 5).await,
            Err(StoreError::StaleEpoch)
        ));
        assert!(matches!(
            release(&s, who, &l).await,
            Err(StoreError::StaleEpoch)
        ));
    }
    assert_eq!(all(&s, "p1").await, vec![l.clone()]);
    assert!(all(&s, "p2").await.is_empty());
    // The same names in the other project are independent: own agent slot, own epoch.
    let twin = granted(claim(&s, "p2", new("t1", "a1", "r1"), 0).await);
    assert_eq!(twin.epoch, 1);
    assert_eq!(all(&s, "p1").await, vec![l]);
}

#[tokio::test]
async fn epochs_survive_for_a_task_with_no_lease_and_never_repeat() {
    let s = FakeStorage::default();
    let mut seen = Vec::new();
    for i in 0..4 {
        let l = granted(claim(&s, P, new("t1", "a1", &format!("r{i}")), 0).await);
        seen.push(l.epoch);
        release(&s, owner(P), &l).await.unwrap();
    }
    assert_eq!(seen, [1, 2, 3, 4]);
}

#[tokio::test]
async fn a_clock_that_would_overflow_is_refused_without_a_change() {
    let s = FakeStorage::default();
    let n = new("t1", "a1", "r1");
    let r = s
        .transaction(owner(P), Lane::Claims, move |u| u.claim_lease(&n, i64::MAX))
        .await;
    assert!(matches!(r, Err(StoreError::CounterExhausted)));
    assert!(all(&s, P).await.is_empty());
    let l = granted(claim(&s, P, new("t1", "a1", "r1"), 0).await);
    assert_eq!(l.epoch, 1, "the refused claim used no epoch");
    assert!(matches!(
        renew(&s, owner(P), &l, i64::MAX).await,
        Err(StoreError::CounterExhausted)
    ));
    assert_eq!(all(&s, P).await, vec![l]);
}

#[tokio::test]
async fn a_task_principal_that_differs_only_in_its_task_is_refused() {
    let s = FakeStorage::default();
    let l = granted(claim(&s, P, new("t1", "a1", "r1"), 0).await);
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
        renew(&s, caller.clone(), &l, 7).await,
        Err(StoreError::StaleEpoch)
    ));
    assert!(matches!(
        release(&s, caller, &l).await,
        Err(StoreError::StaleEpoch)
    ));
    assert_eq!(all(&s, P).await, vec![l]);
}

/// The two lineage checks each refuse a case the other lets through: a token
/// of an earlier lineage against the daemon's current one, and a token of the
/// current lineage against a lease a restore left behind. Only the state can show
/// the first, because the storage's restore ends every lease.
mod lineage_checks {
    use super::*;
    use crate::store::leases::fake::{By, LeaseState};

    fn state() -> (LeaseState, Lease) {
        let mut state = LeaseState::default();
        let ClaimOutcome::Granted(lease) = state.claim("old", &new("t1", "a1", "r1"), 0).unwrap()
        else {
            panic!("claim refused");
        };
        (state, lease)
    }

    #[test]
    fn a_lease_left_behind_by_a_restore_is_refused_under_its_own_lineage() {
        let (state, lease) = state();
        let c = owner(P);
        assert!(state.check(&c, "old", By::Task("t1"), "old", 1).is_ok());
        assert!(matches!(
            state.check(&c, "new", By::Task("t1"), "old", 1),
            Err(StoreError::StaleEpoch)
        ));
        assert!(matches!(
            state.check(&c, "new", By::Run("r1"), "old", lease.epoch),
            Err(StoreError::StaleEpoch)
        ));
    }

    #[test]
    fn a_token_of_the_current_lineage_is_refused_by_a_lease_of_an_earlier_one() {
        // Only the lease's own lineage can refuse this: the token matches the daemon's.
        let (state, _) = state();
        assert!(matches!(
            state.check(&owner(P), "new", By::Task("t1"), "new", 1),
            Err(StoreError::StaleEpoch)
        ));
    }

    #[test]
    fn a_lease_is_found_by_its_run_as_well_as_by_its_task() {
        let (state, lease) = state();
        let found = state
            .check(&owner(P), "old", By::Run("r1"), "old", 1)
            .unwrap();
        assert_eq!(found, &lease);
        assert!(
            state
                .check(&owner(P), "old", By::Run("r2"), "old", 1)
                .is_err()
        );
    }
}
