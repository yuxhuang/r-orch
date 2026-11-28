//! Backend-neutral intent scenarios (R28, R37, R105, R106, R132): typed payloads
//! of ids and hashes, project-local monotonic ids, bounded deterministic pending
//! reads, idempotent completion and bounded retention. Performers are not here:
//! nothing in this port signals a process or touches a directory.
#![allow(
    clippy::unwrap_used,
    reason = "helpers outside #[test] functions; clippy.toml allows unwrap only inside them"
)]

use crate::v2_operations::Fixture;
use orch_core::store::intents::*;
use orch_core::store::leases::{ClaimOutcome, Lease, NewLease};
use orch_core::store::*;

const P: &str = "proj";
const HASH: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn owner(project: &str) -> Caller {
    Caller {
        project: project.into(),
        principal: Principal::Owner {
            identity: "owner".into(),
        },
    }
}

fn task_caller(project: &str, l: &Lease) -> Caller {
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

fn stop(run: &str) -> IntentPayload {
    IntentPayload::ProcessStop { run: run.into() }
}

fn start(run: &str) -> IntentPayload {
    IntentPayload::ProcessStart { run: run.into() }
}

fn remove(task: &str) -> IntentPayload {
    IntentPayload::WorkspaceRemove { task: task.into() }
}

fn blob(hash: &str) -> IntentPayload {
    IntentPayload::BlobDelete { hash: hash.into() }
}

fn new(task: &str, agent: &str, run: &str) -> NewLease {
    NewLease {
        task: task.into(),
        agent: agent.into(),
        run: run.into(),
    }
}

async fn claim(s: &dyn Storage, project: &str, n: NewLease) -> Lease {
    s.transaction(owner(project), Lane::Claims, move |u| {
        match u.claim_lease(&n, 0)? {
            ClaimOutcome::Granted(l) => Ok(l),
            ClaimOutcome::Held(l) => panic!("held by {l:?}"),
        }
    })
    .await
    .unwrap()
}

async fn add(s: &dyn Storage, project: &str, payload: IntentPayload, now: i64) -> Result<Intent> {
    s.transaction(owner(project), Lane::Entities, move |u| {
        u.add_intent(&payload, now)
    })
    .await
}

async fn add_ok(s: &dyn Storage, project: &str, payload: IntentPayload, now: i64) -> Intent {
    add(s, project, payload, now).await.unwrap()
}

async fn finish(s: &dyn Storage, project: &str, id: u64, now: i64) -> Result<FinishOutcome> {
    s.transaction(owner(project), Lane::Entities, move |u| {
        u.finish_intent(id, now)
    })
    .await
}

async fn delete_finished(s: &dyn Storage, project: &str, now: i64, max: usize) -> Result<usize> {
    s.transaction(owner(project), Lane::Maintenance, move |u| {
        u.delete_finished_intents(now, max)
    })
    .await
}

async fn pending(s: &dyn Storage, project: &str) -> Vec<Intent> {
    let snap = s.read(owner(project)).await.unwrap();
    let mut all = vec![];
    let mut after = 0;
    loop {
        let page = snap.read_pending_intents(after, 1_000).await.unwrap();
        let Some(last) = page.last() else {
            return all;
        };
        after = last.id;
        all.extend(page);
    }
}

async fn read(s: &dyn Storage, project: &str, id: u64) -> Option<Intent> {
    s.read(owner(project))
        .await
        .unwrap()
        .read_intent(id)
        .await
        .unwrap()
}

fn is_invalid<T: std::fmt::Debug>(r: Result<T>) -> bool {
    matches!(r, Err(StoreError::InvalidIntent(_)))
}

pub async fn an_intent_is_written_with_its_change(fixture: Fixture) {
    let s = fixture.storage.clone();
    let (lease, intent) = s
        .transaction(owner(P), Lane::Claims, |u| {
            u.create(Entity::Task(Box::new(crate::contract::task("t1"))))?;
            let ClaimOutcome::Granted(lease) = u.claim_lease(&new("t1", "a1", "r1"), 5)? else {
                panic!("fresh claim blocked");
            };
            Ok((lease, u.add_intent(&start("r1"), 1_700_000_000_123)?))
        })
        .await
        .unwrap();
    assert_eq!(intent.id, 1);
    assert_eq!(intent.payload, start("r1"));
    assert_eq!(intent.created_at, 1_700_000_000_123);
    assert_eq!(intent.done_at, None);
    let snap = s.read(owner(P)).await.unwrap();
    assert!(snap.get(EntityKind::Task, "t1").await.unwrap().is_some());
    assert_eq!(snap.read_lease("t1").await.unwrap(), Some(lease));
    assert_eq!(snap.read_intent(1).await.unwrap(), Some(intent.clone()));
    assert_eq!(pending(s.as_ref(), P).await, vec![intent]);
}

pub async fn an_intent_rolls_back_with_its_entity_and_lease(fixture: Fixture) {
    let s = fixture.storage.clone();
    let failed = s
        .transaction(owner(P), Lane::Claims, |u| {
            u.create(Entity::Task(Box::new(crate::contract::task("t1"))))?;
            u.claim_lease(&new("t1", "a1", "r1"), 0)?;
            u.add_intent(&start("r1"), 10)?;
            u.add_intent(&blob(HASH), 11)?;
            Err::<(), _>(StoreError::CounterExhausted)
        })
        .await;
    assert!(matches!(failed, Err(StoreError::CounterExhausted)));
    let snap = s.read(owner(P)).await.unwrap();
    assert!(snap.get(EntityKind::Task, "t1").await.unwrap().is_none());
    assert_eq!(snap.read_lease("t1").await.unwrap(), None);
    assert_eq!(snap.read_intent(1).await.unwrap(), None);
    assert!(pending(s.as_ref(), P).await.is_empty());
    // The id counter rolled back with the rows: nothing was spent.
    assert_eq!(add_ok(s.as_ref(), P, blob(HASH), 12).await.id, 1);
}

pub async fn transaction_reads_see_own_intent_completion_and_roll_back(fixture: Fixture) {
    let s = fixture.storage.clone();
    let failed = s
        .transaction(owner(P), Lane::Entities, |u| {
            // Creation times deliberately oppose id order.
            let first = u.add_intent(&blob(HASH), 30)?;
            let mut middle = u.add_intent(&remove("t1"), 20)?;
            let last = u.add_intent(&remove("t2"), 10)?;
            assert_eq!(u.finish_intent(middle.id, 40)?, FinishOutcome::Finished);
            middle.done_at = Some(40);
            assert_eq!(u.get_intent(middle.id)?, Some(middle));
            assert_eq!(u.list_pending_intents(0, 1)?, vec![first.clone()]);
            assert_eq!(
                u.list_pending_intents(0, 3)?,
                vec![first.clone(), last.clone()]
            );
            assert_eq!(u.list_pending_intents(first.id, 1)?, vec![last.clone()]);
            assert!(u.list_pending_intents(last.id, 1)?.is_empty());
            // Exercise the reads above without ever committing their rows.
            Err::<(), _>(StoreError::CounterExhausted)
        })
        .await;
    assert!(matches!(failed, Err(StoreError::CounterExhausted)));
    let snap = s.read(owner(P)).await.unwrap();
    for id in 1..=3 {
        assert_eq!(snap.read_intent(id).await.unwrap(), None);
    }
    assert!(snap.read_pending_intents(0, 3).await.unwrap().is_empty());
    // The ids rolled back with the rows: nothing was spent.
    assert_eq!(add_ok(s.as_ref(), P, blob(HASH), 50).await.id, 1);
}

pub async fn finish_and_deletion_roll_back_with_their_unit_of_work(fixture: Fixture) {
    let s = fixture.storage.clone();
    add_ok(s.as_ref(), P, blob(HASH), 0).await;
    add_ok(s.as_ref(), P, remove("t1"), 0).await;
    finish(s.as_ref(), P, 2, 10).await.unwrap();
    let failed = s
        .transaction(owner(P), Lane::Entities, |u| {
            u.finish_intent(1, 20)?;
            u.delete_finished_intents(10 + DONE_RETENTION_MS, 10)?;
            Err::<(), _>(StoreError::CounterExhausted)
        })
        .await;
    assert!(matches!(failed, Err(StoreError::CounterExhausted)));
    let first = read(s.as_ref(), P, 1).await.unwrap();
    assert_eq!(first.done_at, None);
    assert_eq!(read(s.as_ref(), P, 2).await.unwrap().done_at, Some(10));
    assert_eq!(pending(s.as_ref(), P).await, vec![first]);
}

pub async fn an_error_caught_inside_the_callback_changes_nothing(fixture: Fixture) {
    let s = fixture.storage.clone();
    claim(s.as_ref(), P, new("t1", "a1", "r1")).await;
    // Every refusal below is caught and the callback commits. The refusals must
    // have left no row, no time, no id and no half-applied step behind.
    let (counter_probe, refused) = s
        .transaction(owner(P), Lane::Entities, |u| {
            u.create(Entity::Task(Box::new(crate::contract::task("marker"))))?;
            let refusals = [
                u.add_intent(&remove("../escape"), 1).map(drop),
                u.add_intent(&blob("not-a-hash"), 1).map(drop),
                u.add_intent(&stop("r-without-lease"), 1).map(drop),
                u.add_intent(&start("r-without-lease"), 1).map(drop),
                u.finish_intent(0, 1).map(drop),
                u.finish_intent(99, 1).map(drop),
                u.delete_finished_intents(1, 0).map(drop),
                u.delete_finished_intents(1, 1_001).map(drop),
                u.list_pending_intents(0, 0).map(drop),
            ];
            let refused = refusals.iter().filter(|r| r.is_err()).count();
            Ok((u.add_intent(&stop("r1"), 2)?, refused))
        })
        .await
        .unwrap();
    assert_eq!(refused, 9);
    // The first intent of the project is id 1: no refusal spent an id.
    assert_eq!(counter_probe.id, 1);
    assert_eq!(pending(s.as_ref(), P).await, vec![counter_probe]);
    let snap = s.read(owner(P)).await.unwrap();
    assert!(
        snap.get(EntityKind::Task, "marker")
            .await
            .unwrap()
            .is_some()
    );
}

pub async fn ids_are_monotonic_and_never_reused(fixture: Fixture) {
    let s = fixture.storage.clone();
    for expected in 1..=3 {
        assert_eq!(add_ok(s.as_ref(), P, blob(HASH), 0).await.id, expected);
    }
    for id in 1..=3 {
        finish(s.as_ref(), P, id, 100).await.unwrap();
    }
    assert_eq!(
        delete_finished(s.as_ref(), P, 100 + DONE_RETENTION_MS, 10)
            .await
            .unwrap(),
        3
    );
    assert!(read(s.as_ref(), P, 3).await.is_none());
    // Deleting every row does not free an id.
    assert_eq!(add_ok(s.as_ref(), P, blob(HASH), 0).await.id, 4);
}

pub async fn pending_reads_are_ordered_bounded_and_resumable(fixture: Fixture) {
    let s = fixture.storage.clone();
    s.transaction(owner(P), Lane::Entities, |u| {
        for n in 0..5 {
            u.add_intent(&remove(&format!("t{n}")), n)?;
        }
        Ok(())
    })
    .await
    .unwrap();
    finish(s.as_ref(), P, 2, 9).await.unwrap();
    let snap = s.read(owner(P)).await.unwrap();
    let ids = |v: Vec<Intent>| v.into_iter().map(|i| i.id).collect::<Vec<_>>();
    assert_eq!(
        ids(snap.read_pending_intents(0, 2).await.unwrap()),
        vec![1, 3]
    );
    assert_eq!(
        ids(snap.read_pending_intents(3, 10).await.unwrap()),
        vec![4, 5]
    );
    assert!(snap.read_pending_intents(5, 10).await.unwrap().is_empty());
    assert!(is_invalid(snap.read_pending_intents(0, 0).await));
    assert!(is_invalid(snap.read_pending_intents(0, 1_001).await));
    // The same snapshot reads the same rows however many times it is asked.
    s.transaction(owner(P), Lane::Entities, |u| {
        u.finish_intent(1, 10).map(drop)
    })
    .await
    .unwrap();
    assert_eq!(
        ids(snap.read_pending_intents(0, 100).await.unwrap()),
        vec![1, 3, 4, 5]
    );
    let fresh = s.read(owner(P)).await.unwrap();
    assert_eq!(
        ids(fresh.read_pending_intents(0, 100).await.unwrap()),
        vec![3, 4, 5]
    );
}

pub async fn a_backlog_over_the_read_bound_is_enumerated_completely(fixture: Fixture) {
    let s = fixture.storage.clone();
    s.transaction(owner(P), Lane::Entities, |u| {
        for _ in 0..1_001 {
            u.add_intent(&blob(HASH), 0)?;
        }
        Ok(())
    })
    .await
    .unwrap();
    let all = pending(s.as_ref(), P).await;
    assert_eq!(all.len(), 1_001);
    assert!(all.iter().zip(1..).all(|(i, id)| i.id == id));
}

pub async fn finishing_is_idempotent_and_the_first_time_wins(fixture: Fixture) {
    let s = fixture.storage.clone();
    add_ok(s.as_ref(), P, remove("t1"), 1).await;
    assert_eq!(
        finish(s.as_ref(), P, 1, 5_000).await.unwrap(),
        FinishOutcome::Finished
    );
    assert_eq!(
        finish(s.as_ref(), P, 1, 9_000).await.unwrap(),
        FinishOutcome::AlreadyDone
    );
    let done = read(s.as_ref(), P, 1).await.unwrap();
    assert_eq!(done.done_at, Some(5_000));
    assert!(pending(s.as_ref(), P).await.is_empty());
    // Twice in one transaction too.
    add_ok(s.as_ref(), P, remove("t2"), 1).await;
    let outcomes = s
        .transaction(owner(P), Lane::Entities, |u| {
            Ok((u.finish_intent(2, 7)?, u.finish_intent(2, 8)?))
        })
        .await
        .unwrap();
    assert_eq!(
        outcomes,
        (FinishOutcome::Finished, FinishOutcome::AlreadyDone)
    );
    assert_eq!(read(s.as_ref(), P, 2).await.unwrap().done_at, Some(7));
}

pub async fn an_id_that_was_never_issued_is_refused_not_ignored(fixture: Fixture) {
    let s = fixture.storage.clone();
    add_ok(s.as_ref(), P, remove("t1"), 1).await;
    assert!(is_invalid(finish(s.as_ref(), P, 0, 1).await));
    assert!(is_invalid(finish(s.as_ref(), P, 2, 1).await));
    assert!(is_invalid(finish(s.as_ref(), P, u64::MAX, 1).await));
    assert_eq!(read(s.as_ref(), P, 1).await.unwrap().done_at, None);
    assert!(read(s.as_ref(), P, 0).await.is_none());
}

pub async fn a_finished_and_deleted_intent_finishes_again_as_already_done(fixture: Fixture) {
    let s = fixture.storage.clone();
    add_ok(s.as_ref(), P, remove("t1"), 1).await;
    finish(s.as_ref(), P, 1, 10).await.unwrap();
    delete_finished(s.as_ref(), P, 10 + DONE_RETENTION_MS, 10)
        .await
        .unwrap();
    assert!(read(s.as_ref(), P, 1).await.is_none());
    assert_eq!(
        finish(s.as_ref(), P, 1, 99).await.unwrap(),
        FinishOutcome::AlreadyDone
    );
    assert!(read(s.as_ref(), P, 1).await.is_none());
}

pub async fn the_kinds_say_whether_a_restart_repeats_them(_: Fixture) {
    assert!(!start("r").kind().repeats_after_restart());
    assert!(stop("r").kind().repeats_after_restart());
    assert!(remove("t").kind().repeats_after_restart());
    assert!(blob(HASH).kind().repeats_after_restart());
    let names: Vec<_> = [start("r"), stop("r"), remove("t"), blob(HASH)]
        .iter()
        .map(|p| p.kind().as_str())
        .collect();
    assert_eq!(
        names,
        [
            "process.start",
            "process.stop",
            "workspace.remove",
            "blob.delete"
        ]
    );
}

pub async fn a_started_intent_stays_pending_until_recovery_finishes_it(fixture: Fixture) {
    // The contract keeps a process.start pending, whatever happened to its
    // process: it records and never performs. Whether a pending one is
    // performed again is the kind's rule (never) and the performer's to obey.
    let s = fixture.storage.clone();
    claim(s.as_ref(), P, new("t1", "a1", "r1")).await;
    let intent = add_ok(s.as_ref(), P, start("r1"), 3).await;
    fixture.restore.restore_to_lineage("after-restore").await;
    assert_eq!(pending(s.as_ref(), P).await, vec![intent.clone()]);
    assert!(!intent.payload.kind().repeats_after_restart());
    finish(s.as_ref(), P, intent.id, 4).await.unwrap();
    assert!(pending(s.as_ref(), P).await.is_empty());
}

pub async fn a_process_intent_needs_the_run_of_a_lease(fixture: Fixture) {
    let s = fixture.storage.clone();
    for payload in [stop("r1"), start("r1")] {
        assert!(matches!(
            add(s.as_ref(), P, payload, 1).await,
            Err(StoreError::StaleEpoch)
        ));
    }
    let lease = claim(s.as_ref(), P, new("t1", "a1", "r1")).await;
    // Another run's name is no lease, even when the task holds one.
    assert!(matches!(
        add(s.as_ref(), P, stop("r2"), 1).await,
        Err(StoreError::StaleEpoch)
    ));
    assert!(pending(s.as_ref(), P).await.is_empty());
    assert_eq!(add_ok(s.as_ref(), P, stop("r1"), 1).await.id, 1);
    assert_eq!(add_ok(s.as_ref(), P, start("r1"), 1).await.id, 2);
    // Cancel (R37): the intent is written, then the lease is released, in one
    // transaction. The other order is refused and changes nothing.
    s.transaction(owner(P), Lane::Entities, {
        let l = lease.clone();
        move |u| {
            u.add_intent(&stop("r1"), 2)?;
            u.release_lease("t1", &l.lineage, l.epoch)?;
            Ok(())
        }
    })
    .await
    .unwrap();
    assert_eq!(pending(s.as_ref(), P).await.len(), 3);
    assert!(matches!(
        add(s.as_ref(), P, stop("r1"), 3).await,
        Err(StoreError::StaleEpoch)
    ));
    assert_eq!(pending(s.as_ref(), P).await.len(), 3);
}

pub async fn releasing_first_then_adding_a_stop_is_refused_without_a_trace(fixture: Fixture) {
    let s = fixture.storage.clone();
    let lease = claim(s.as_ref(), P, new("t1", "a1", "r1")).await;
    let failed = s
        .transaction(owner(P), Lane::Entities, move |u| {
            u.release_lease("t1", &lease.lineage, lease.epoch)?;
            u.add_intent(&stop("r1"), 1)?;
            Ok(())
        })
        .await;
    assert!(matches!(failed, Err(StoreError::StaleEpoch)));
    // The release rolled back with the refused intent.
    assert!(
        s.read(owner(P))
            .await
            .unwrap()
            .read_lease("t1")
            .await
            .unwrap()
            .is_some()
    );
    assert!(pending(s.as_ref(), P).await.is_empty());
}

pub async fn a_task_principal_may_stop_only_its_own_run(fixture: Fixture) {
    let s = fixture.storage.clone();
    let mine = claim(s.as_ref(), P, new("t1", "a1", "r1")).await;
    let other = claim(s.as_ref(), P, new("t2", "a2", "r2")).await;
    let add_as = |who: Caller, payload: IntentPayload| {
        let s = s.clone();
        async move {
            s.transaction(who, Lane::Entities, move |u| u.add_intent(&payload, 1))
                .await
        }
    };
    assert_eq!(
        add_as(task_caller(P, &mine), stop("r1")).await.unwrap().id,
        1
    );
    for payload in [stop("r2"), start("r2")] {
        assert!(matches!(
            add_as(task_caller(P, &mine), payload).await,
            Err(StoreError::StaleEpoch)
        ));
    }
    // A principal whose token is not the lease's is refused for its own run.
    let mut stale = task_caller(P, &mine);
    if let Principal::Task { epoch, .. } = &mut stale.principal {
        *epoch += 1;
    }
    assert!(matches!(
        add_as(stale, stop("r1")).await,
        Err(StoreError::StaleEpoch)
    ));
    let mut other_lineage = task_caller(P, &mine);
    if let Principal::Task { lineage, .. } = &mut other_lineage.principal {
        *lineage = "earlier".into();
    }
    assert!(matches!(
        add_as(other_lineage, stop("r1")).await,
        Err(StoreError::StaleEpoch)
    ));
    // The owner may name either run, and the neighbour's lease is untouched.
    assert_eq!(add_ok(s.as_ref(), P, stop("r2"), 1).await.id, 2);
    assert_eq!(pending(s.as_ref(), P).await.len(), 2);
    assert_eq!(
        s.read(owner(P)).await.unwrap().read_leases().await.unwrap(),
        vec![mine, other]
    );
}

pub async fn a_restore_ends_the_right_to_stop_but_keeps_what_was_written(fixture: Fixture) {
    let s = fixture.storage.clone();
    claim(s.as_ref(), P, new("t1", "a1", "r1")).await;
    let written = add_ok(s.as_ref(), P, stop("r1"), 1).await;
    fixture.restore.restore_to_lineage("after-restore").await;
    assert!(matches!(
        add(s.as_ref(), P, stop("r1"), 2).await,
        Err(StoreError::StaleEpoch)
    ));
    assert_eq!(pending(s.as_ref(), P).await, vec![written]);
    // Ids go on; workspace and blob intents need no lease.
    assert_eq!(add_ok(s.as_ref(), P, remove("t1"), 3).await.id, 2);
    assert_eq!(add_ok(s.as_ref(), P, blob(HASH), 3).await.id, 3);
}

pub async fn payloads_hold_ids_and_hashes_never_paths(fixture: Fixture) {
    let s = fixture.storage.clone();
    claim(s.as_ref(), P, new("t1", "a1", "r1")).await;
    let long = "x".repeat(MAX_ID_BYTES + 1);
    let bad_names = [
        "",
        ".",
        "..",
        "../outside",
        "a/b",
        "/abs",
        "a\\b",
        "a b",
        "tsk\0x",
        "tsk\n",
        "tsk~",
        "tsk:1",
        "t\u{e9}",
        &long,
    ];
    for name in bad_names {
        assert!(
            is_invalid(add(s.as_ref(), P, remove(name), 1).await),
            "{name:?}"
        );
        assert!(
            is_invalid(add(s.as_ref(), P, stop(name), 1).await),
            "{name:?}"
        );
        assert!(
            is_invalid(add(s.as_ref(), P, start(name), 1).await),
            "{name:?}"
        );
    }
    let bad_hashes = [
        "",
        "abc",
        &HASH[1..],
        &format!("{HASH}0"),
        &HASH.to_uppercase(),
        &format!("{}g", &HASH[1..]),
        "../../etc/passwd",
        &format!("sha256/{HASH}"),
    ];
    for hash in bad_hashes {
        assert!(
            is_invalid(add(s.as_ref(), P, blob(hash), 1).await),
            "{hash:?}"
        );
    }
    assert!(pending(s.as_ref(), P).await.is_empty());
    // One name of the allowed alphabet is accepted, at its longest too.
    let longest = "a".repeat(MAX_ID_BYTES);
    for name in ["tsk_A.b-c", "run_0", "...x", longest.as_str()] {
        add_ok(s.as_ref(), P, remove(name), 1).await;
    }
    assert_eq!(add_ok(s.as_ref(), P, blob(HASH), 1).await.id, 5);
}

pub async fn completed_intents_are_kept_for_exactly_the_retention(fixture: Fixture) {
    let s = fixture.storage.clone();
    let done_at = 1_000;
    add_ok(s.as_ref(), P, blob(HASH), 0).await;
    finish(s.as_ref(), P, 1, done_at).await.unwrap();
    let cutoff = done_at + DONE_RETENTION_MS;
    assert_eq!(DONE_RETENTION_MS, 7 * 24 * 60 * 60 * 1_000);
    assert_eq!(
        delete_finished(s.as_ref(), P, cutoff - 1, 10)
            .await
            .unwrap(),
        0
    );
    assert!(read(s.as_ref(), P, 1).await.is_some());
    assert_eq!(delete_finished(s.as_ref(), P, cutoff, 10).await.unwrap(), 1);
    assert!(read(s.as_ref(), P, 1).await.is_none());
}

pub async fn retention_never_underflows_below_the_signed_minimum(fixture: Fixture) {
    // One policy for every backend: a row is old enough when
    // `done_at <= now - DONE_RETENTION_MS`, and when that subtraction would pass
    // i64::MIN nothing is old enough. Saturating instead would delete the row
    // finished at i64::MIN for every clock below i64::MIN + the retention.
    let s = fixture.storage.clone();
    let first = i64::MIN + DONE_RETENTION_MS;
    add_ok(s.as_ref(), P, blob(HASH), i64::MIN).await;
    add_ok(s.as_ref(), P, blob(HASH), i64::MIN).await;
    add_ok(s.as_ref(), P, blob(HASH), i64::MIN).await;
    finish(s.as_ref(), P, 1, i64::MIN).await.unwrap();
    finish(s.as_ref(), P, 2, i64::MIN + 1).await.unwrap();
    assert_eq!(
        read(s.as_ref(), P, 1).await.unwrap().done_at,
        Some(i64::MIN)
    );
    for now in [i64::MIN, i64::MIN + 1, first - 1] {
        assert_eq!(delete_finished(s.as_ref(), P, now, 10).await.unwrap(), 0);
        assert!(read(s.as_ref(), P, 1).await.is_some(), "now {now}");
    }
    // Exactly seven days after the minimum, only the row done at the minimum.
    assert_eq!(delete_finished(s.as_ref(), P, first, 10).await.unwrap(), 1);
    assert!(read(s.as_ref(), P, 1).await.is_none());
    assert!(read(s.as_ref(), P, 2).await.is_some());
    assert_eq!(
        delete_finished(s.as_ref(), P, first + 1, 10).await.unwrap(),
        1
    );
    // The pending row, also created at the minimum, stays however late it is.
    assert_eq!(
        delete_finished(s.as_ref(), P, i64::MAX, 10).await.unwrap(),
        0
    );
    assert_eq!(pending(s.as_ref(), P).await.len(), 1);
    // The top of the range is exact too: nothing finished at i64::MAX is old.
    finish(s.as_ref(), P, 3, i64::MAX).await.unwrap();
    assert_eq!(
        delete_finished(s.as_ref(), P, i64::MAX, 10).await.unwrap(),
        0
    );
    assert!(read(s.as_ref(), P, 3).await.is_some());
}

pub async fn ids_past_the_signed_range_read_as_absent(fixture: Fixture) {
    let s = fixture.storage.clone();
    add_ok(s.as_ref(), P, blob(HASH), 1).await;
    for id in [i64::MAX as u64, i64::MAX as u64 + 1, u64::MAX] {
        assert!(read(s.as_ref(), P, id).await.is_none(), "{id}");
        let snap = s.read(owner(P)).await.unwrap();
        assert!(snap.read_pending_intents(id, 1).await.unwrap().is_empty());
        assert!(is_invalid(finish(s.as_ref(), P, id, 2).await), "{id}");
    }
    assert_eq!(pending(s.as_ref(), P).await.len(), 1);
}

pub async fn a_pending_intent_is_never_deleted_by_retention(fixture: Fixture) {
    let s = fixture.storage.clone();
    add_ok(s.as_ref(), P, blob(HASH), 0).await;
    assert_eq!(
        delete_finished(s.as_ref(), P, i64::MAX, 10).await.unwrap(),
        0
    );
    assert_eq!(pending(s.as_ref(), P).await.len(), 1);
}

pub async fn retention_deletes_the_oldest_ids_first_within_its_bound(fixture: Fixture) {
    let s = fixture.storage.clone();
    s.transaction(owner(P), Lane::Entities, |u| {
        for _ in 0..5 {
            u.add_intent(&blob(HASH), 0)?;
        }
        // 5 finished first, then 1 and 3, so id order and time order differ.
        u.finish_intent(5, 1)?;
        u.finish_intent(1, 2)?;
        u.finish_intent(3, 3)?;
        Ok(())
    })
    .await
    .unwrap();
    let late = i64::MAX / 2;
    assert_eq!(delete_finished(s.as_ref(), P, late, 2).await.unwrap(), 2);
    assert!(read(s.as_ref(), P, 1).await.is_none());
    assert!(read(s.as_ref(), P, 3).await.is_none());
    assert!(read(s.as_ref(), P, 5).await.is_some());
    assert_eq!(delete_finished(s.as_ref(), P, late, 2).await.unwrap(), 1);
    assert_eq!(delete_finished(s.as_ref(), P, late, 2).await.unwrap(), 0);
    let left: Vec<_> = pending(s.as_ref(), P).await.iter().map(|i| i.id).collect();
    assert_eq!(left, vec![2, 4]);
}

pub async fn retention_over_the_bound_takes_several_calls(fixture: Fixture) {
    let s = fixture.storage.clone();
    s.transaction(owner(P), Lane::Entities, |u| {
        for id in 1..=1_001 {
            u.add_intent(&blob(HASH), 0)?;
            u.finish_intent(id, 0)?;
        }
        Ok(())
    })
    .await
    .unwrap();
    assert_eq!(
        delete_finished(s.as_ref(), P, DONE_RETENTION_MS, 1_000)
            .await
            .unwrap(),
        1_000
    );
    assert!(read(s.as_ref(), P, 1_001).await.is_some());
    assert_eq!(
        delete_finished(s.as_ref(), P, DONE_RETENTION_MS, 1_000)
            .await
            .unwrap(),
        1
    );
}

pub async fn another_project_sees_no_intent_and_changes_none(fixture: Fixture) {
    let s = fixture.storage.clone();
    add_ok(s.as_ref(), P, blob(HASH), 1).await;
    add_ok(s.as_ref(), P, remove("t1"), 1).await;
    assert!(pending(s.as_ref(), "other").await.is_empty());
    assert!(read(s.as_ref(), "other", 1).await.is_none());
    // Ids are project-local: the other project starts again at 1, and
    // finishing, deleting and reading there never reach this project's rows.
    assert!(is_invalid(finish(s.as_ref(), "other", 1, 5).await));
    assert_eq!(add_ok(s.as_ref(), "other", remove("t9"), 2).await.id, 1);
    assert_eq!(
        finish(s.as_ref(), "other", 1, 5).await.unwrap(),
        FinishOutcome::Finished
    );
    assert_eq!(
        delete_finished(s.as_ref(), "other", i64::MAX, 10)
            .await
            .unwrap(),
        1
    );
    let mine = pending(s.as_ref(), P).await;
    assert_eq!(mine.len(), 2);
    assert!(mine.iter().all(|i| i.done_at.is_none()));
    assert_eq!(mine[0].payload, blob(HASH));
    // The shared task ID has different runs in the two projects.
    claim(s.as_ref(), P, new("t1", "a1", "r2")).await;
    claim(s.as_ref(), "other", new("t1", "a1", "r1")).await;
    // Catch the refusal and commit, so a counter accidentally spent by a
    // refused operation cannot hide behind the outer transaction rollback.
    s.transaction(owner(P), Lane::Entities, |u| {
        assert!(matches!(
            u.add_intent(&stop("r1"), 1),
            Err(StoreError::StaleEpoch)
        ));
        Ok(())
    })
    .await
    .unwrap();
    assert_eq!(add_ok(s.as_ref(), P, remove("t1"), 2).await.id, 3);
    assert_eq!(pending(s.as_ref(), P).await.len(), 3);
    assert_eq!(add_ok(s.as_ref(), P, stop("r2"), 3).await.id, 4);
}

pub async fn concurrent_writers_get_distinct_gapless_ids(fixture: Fixture) {
    let s = fixture.storage.clone();
    let one = |n: i64| {
        let s = s.clone();
        async move { add_ok(s.as_ref(), P, blob(HASH), n).await.id }
    };
    let (a, b, c) = tokio::join!(one(1), one(2), one(3));
    let mut ids = [a, b, c];
    ids.sort();
    assert_eq!(ids, [1, 2, 3]);
    assert_eq!(pending(s.as_ref(), P).await.len(), 3);
}

macro_rules! intent_suite {
    ($module:ident, $factory:path) => {
        mod $module {
            macro_rules! case {
                ($name:ident) => {
                    #[tokio::test]
                    async fn $name() {
                        crate::intents_v2::$name($factory().await).await;
                    }
                };
            }
            case!(an_intent_is_written_with_its_change);
            case!(an_intent_rolls_back_with_its_entity_and_lease);
            case!(transaction_reads_see_own_intent_completion_and_roll_back);
            case!(finish_and_deletion_roll_back_with_their_unit_of_work);
            case!(an_error_caught_inside_the_callback_changes_nothing);
            case!(ids_are_monotonic_and_never_reused);
            case!(pending_reads_are_ordered_bounded_and_resumable);
            case!(a_backlog_over_the_read_bound_is_enumerated_completely);
            case!(finishing_is_idempotent_and_the_first_time_wins);
            case!(an_id_that_was_never_issued_is_refused_not_ignored);
            case!(a_finished_and_deleted_intent_finishes_again_as_already_done);
            case!(the_kinds_say_whether_a_restart_repeats_them);
            case!(a_started_intent_stays_pending_until_recovery_finishes_it);
            case!(a_process_intent_needs_the_run_of_a_lease);
            case!(releasing_first_then_adding_a_stop_is_refused_without_a_trace);
            case!(a_task_principal_may_stop_only_its_own_run);
            case!(a_restore_ends_the_right_to_stop_but_keeps_what_was_written);
            case!(payloads_hold_ids_and_hashes_never_paths);
            case!(completed_intents_are_kept_for_exactly_the_retention);
            case!(retention_never_underflows_below_the_signed_minimum);
            case!(ids_past_the_signed_range_read_as_absent);
            case!(a_pending_intent_is_never_deleted_by_retention);
            case!(retention_deletes_the_oldest_ids_first_within_its_bound);
            case!(retention_over_the_bound_takes_several_calls);
            case!(another_project_sees_no_intent_and_changes_none);
            case!(concurrent_writers_get_distinct_gapless_ids);
        }
    };
}
pub(crate) use intent_suite;
