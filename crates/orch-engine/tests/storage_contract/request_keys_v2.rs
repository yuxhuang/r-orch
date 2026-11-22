//! Backend-neutral request-key scenarios (R45, R46, R47, R134): a change keyed
//! once runs once, key and change commit together, the whole scope isolates, a
//! key is replayable for exactly one hour, a large result is not kept and expiry
//! is bounded. The scenarios play the consumer: they authorize nothing (the
//! dispatcher does that first, T17) and hash nothing.
#![allow(
    clippy::unwrap_used,
    reason = "helpers outside #[test] functions; clippy.toml allows unwrap only inside them"
)]

use crate::v2_operations::Fixture;
use orch_core::store::request_keys::*;
use orch_core::store::*;

const P: &str = "proj";
const OP: &str = "task.update";
const HOUR: i64 = 3_600_000;
const T0: i64 = 1_700_000_000_000;

fn owner(project: &str, identity: &str) -> Caller {
    Caller {
        project: project.into(),
        principal: Principal::Owner {
            identity: identity.into(),
        },
    }
}

fn task_principal(project: &str, run: &str, epoch: u64) -> Caller {
    Caller {
        project: project.into(),
        principal: Principal::Task {
            identity: "t".into(),
            task: "t1".into(),
            run: run.into(),
            lineage: "initial-lineage".into(),
            epoch,
        },
    }
}

fn me() -> Caller {
    owner(P, "owner")
}

/// A key of the minimum length, made from `n`.
fn key(n: &str) -> String {
    format!("{n:_<16}")
}

fn hash(c: char) -> String {
    c.to_string().repeat(64)
}

fn is_invalid<T: std::fmt::Debug>(r: Result<T>) -> bool {
    matches!(r, Err(StoreError::InvalidRequestKey(_)))
}

async fn seed(s: &dyn Storage, project: &str) {
    s.transaction(me_in(project), Lane::Entities, |u| {
        u.create(Entity::Task(Box::new(crate::contract::task("t1"))))?;
        Ok(())
    })
    .await
    .unwrap();
}

fn me_in(project: &str) -> Caller {
    owner(project, "owner")
}

/// How many times the change has run: the task's revision, 1 when seeded.
async fn runs(s: &dyn Storage, project: &str) -> u64 {
    s.read(me_in(project))
        .await
        .unwrap()
        .get(EntityKind::Task, "t1")
        .await
        .unwrap()
        .unwrap()
        .revision
        .get()
        - 1
}

#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    Ran,
    Replayed(String),
    ReplayedWithoutResult,
    Conflict,
}

/// One keyed request as the consumer would make it: look the key up, and only
/// for a new one make the change and store the key beside it.
fn keyed(
    u: &mut dyn UnitOfWork,
    key: &str,
    op: &str,
    req: &str,
    result: &str,
    now: i64,
) -> Result<Outcome> {
    let scope = RequestScope::of(u.caller(), key)?;
    Ok(match u.check_request_key(&scope, op, req, now)? {
        KeyCheck::New => {
            let record = u.get(EntityKind::Task, "t1")?.unwrap();
            u.update(record.entity, record.revision)?;
            u.store_request_key(&scope, op, req, result, now)?;
            Outcome::Ran
        }
        KeyCheck::Replay(first) => Outcome::Replayed(first),
        KeyCheck::ReplayWithoutResult => Outcome::ReplayedWithoutResult,
        KeyCheck::Mismatch => Outcome::Conflict,
    })
}

async fn request(
    s: &dyn Storage,
    caller: Caller,
    key: &str,
    op: &str,
    req: &str,
    result: &str,
    now: i64,
) -> Result<Outcome> {
    let (key, op, req, result) = (
        key.to_string(),
        op.to_string(),
        req.to_string(),
        result.to_string(),
    );
    s.transaction(caller, Lane::Entities, move |u| {
        keyed(u, &key, &op, &req, &result, now)
    })
    .await
}

async fn expire(s: &dyn Storage, project: &str, now: i64, max: usize) -> Result<usize> {
    s.transaction(me_in(project), Lane::Maintenance, move |u| {
        u.delete_expired_request_keys(now, max)
    })
    .await
}

pub async fn a_repeated_change_runs_once(fixture: Fixture) {
    let s = fixture.storage.clone();
    seed(s.as_ref(), P).await;
    let k = key("once");
    let first = request(s.as_ref(), me(), &k, OP, &hash('a'), "first", T0).await;
    assert_eq!(first.unwrap(), Outcome::Ran);
    // The reply was lost: the same request again, later and again, changes nothing.
    for later in [T0, T0 + 1, T0 + 60_000] {
        let again = request(s.as_ref(), me(), &k, OP, &hash('a'), "second", later).await;
        assert_eq!(again.unwrap(), Outcome::Replayed("first".into()));
    }
    assert_eq!(runs(s.as_ref(), P).await, 1);
    // Another key is another request.
    let other = request(s.as_ref(), me(), &key("two"), OP, &hash('a'), "x", T0).await;
    assert_eq!(other.unwrap(), Outcome::Ran);
    assert_eq!(runs(s.as_ref(), P).await, 2);
}

pub async fn the_same_key_with_another_operation_or_hash_is_a_mismatch(fixture: Fixture) {
    let s = fixture.storage.clone();
    seed(s.as_ref(), P).await;
    let k = key("mismatch");
    request(s.as_ref(), me(), &k, OP, &hash('a'), "first", T0)
        .await
        .unwrap();
    for (op, req) in [
        (OP, hash('b')),
        ("task.delete", hash('a')),
        ("task.delete", hash('b')),
    ] {
        let refused = request(s.as_ref(), me(), &k, op, &req, "other", T0 + 1).await;
        assert_eq!(refused.unwrap(), Outcome::Conflict, "{op} {req}");
    }
    // Nothing ran and nothing was replaced: the first request still replays.
    assert_eq!(runs(s.as_ref(), P).await, 1);
    let again = request(s.as_ref(), me(), &k, OP, &hash('a'), "x", T0 + 2).await;
    assert_eq!(again.unwrap(), Outcome::Replayed("first".into()));
}

pub async fn a_store_over_a_live_key_is_refused_and_changes_nothing_even_if_caught(
    fixture: Fixture,
) {
    let s = fixture.storage.clone();
    seed(s.as_ref(), P).await;
    let k = key("live");
    request(s.as_ref(), me(), &k, OP, &hash('a'), "first", T0)
        .await
        .unwrap();
    // A caller that skipped the check and stores a different body, then catches
    // the refusal and commits an entity change beside it.
    let kept = k.clone();
    s.transaction(me(), Lane::Entities, move |u| {
        let scope = RequestScope::of(u.caller(), &kept)?;
        for (op, req, result) in [
            (OP, hash('b'), "other"),
            ("task.delete", hash('a'), "other"),
            (OP, hash('a'), "same-body-again"),
        ] {
            assert!(is_invalid(u.store_request_key(
                &scope,
                op,
                &req,
                result,
                T0 + 5
            )));
        }
        let record = u.get(EntityKind::Task, "t1")?.unwrap();
        u.update(record.entity, record.revision)?;
        Ok(())
    })
    .await
    .unwrap();
    let again = request(s.as_ref(), me(), &k, OP, &hash('a'), "x", T0 + 6).await;
    assert_eq!(again.unwrap(), Outcome::Replayed("first".into()));
    // Only the caught transaction's entity change was kept: run 1 plus that one.
    assert_eq!(runs(s.as_ref(), P).await, 2);
}

pub async fn key_and_change_commit_or_roll_back_together(fixture: Fixture) {
    let s = fixture.storage.clone();
    seed(s.as_ref(), P).await;
    let k = key("rollback");
    let failed = {
        let k = k.clone();
        s.transaction(me(), Lane::Entities, move |u| {
            assert_eq!(keyed(u, &k, OP, &hash('a'), "lost", T0)?, Outcome::Ran);
            // The key is visible to its own unit of work, as the change is.
            assert_eq!(
                keyed(u, &k, OP, &hash('a'), "again", T0)?,
                Outcome::Replayed("lost".into())
            );
            Err::<(), _>(StoreError::CounterExhausted)
        })
        .await
    };
    assert!(matches!(failed, Err(StoreError::CounterExhausted)));
    // Neither the change nor the key survived, so the retry is a new request.
    assert_eq!(runs(s.as_ref(), P).await, 0);
    let retried = request(s.as_ref(), me(), &k, OP, &hash('a'), "kept", T0 + 1).await;
    assert_eq!(retried.unwrap(), Outcome::Ran);
    assert_eq!(runs(s.as_ref(), P).await, 1);
    // A refusal of the change itself leaves no key either.
    let refused = {
        let k = key("refused");
        s.transaction(me(), Lane::Entities, move |u| {
            let scope = RequestScope::of(u.caller(), &k)?;
            u.store_request_key(&scope, OP, &hash('a'), "x", T0)?;
            u.update(
                Entity::Task(Box::new(crate::contract::task("t1"))),
                Revision::ONE,
            )?;
            Ok(())
        })
        .await
    };
    assert!(matches!(refused, Err(StoreError::StaleRevision { .. })));
    let after = request(
        s.as_ref(),
        me(),
        &key("refused"),
        OP,
        &hash('a'),
        "y",
        T0 + 2,
    )
    .await;
    assert_eq!(after.unwrap(), Outcome::Ran);
}

pub async fn concurrent_repeats_run_the_change_once(fixture: Fixture) {
    let s = fixture.storage.clone();
    seed(s.as_ref(), P).await;
    let k = key("race");
    let one = |n: i64| {
        let (s, k) = (s.clone(), k.clone());
        async move {
            request(
                s.as_ref(),
                me(),
                &k,
                OP,
                &hash('a'),
                &format!("r{n}"),
                T0 + n,
            )
            .await
            .unwrap()
        }
    };
    let (a, b, c, d) = tokio::join!(one(1), one(2), one(3), one(4));
    let all = [a, b, c, d];
    let ran: Vec<usize> = (0..4).filter(|&i| all[i] == Outcome::Ran).collect();
    assert_eq!(ran.len(), 1, "{all:?}");
    // Every other repeat got the one stored result.
    let winner = format!("r{}", ran[0] + 1);
    for (i, outcome) in all.iter().enumerate() {
        if i != ran[0] {
            assert_eq!(*outcome, Outcome::Replayed(winner.clone()));
        }
    }
    assert_eq!(runs(s.as_ref(), P).await, 1);
}

pub async fn a_key_is_not_shared_across_identity_project_run_or_epoch(fixture: Fixture) {
    let s = fixture.storage.clone();
    for project in [P, "other"] {
        seed(s.as_ref(), project).await;
    }
    let k = key("shared");
    let callers = [
        me(),
        owner(P, "someone-else"),
        owner("other", "owner"),
        // A task principal is not an owner of the same identity, whatever the key.
        task_principal(P, "r1", 1),
        task_principal(P, "r2", 1),
        task_principal(P, "r1", 2),
        task_principal("other", "r1", 1),
    ];
    // Each caller's first request with the one key runs; none replays another's.
    for (n, caller) in callers.iter().enumerate() {
        let first = request(
            s.as_ref(),
            caller.clone(),
            &k,
            OP,
            &hash('a'),
            &format!("c{n}"),
            T0,
        )
        .await;
        assert_eq!(first.unwrap(), Outcome::Ran, "caller {n}");
    }
    assert_eq!(runs(s.as_ref(), P).await, 5);
    assert_eq!(runs(s.as_ref(), "other").await, 2);
    // And each replays only its own result, even with a body that conflicts elsewhere.
    for (n, caller) in callers.iter().enumerate() {
        let again = request(s.as_ref(), caller.clone(), &k, OP, &hash('a'), "x", T0 + 1).await;
        assert_eq!(
            again.unwrap(),
            Outcome::Replayed(format!("c{n}")),
            "caller {n}"
        );
    }
    // Expiry in one project reaches no other.
    assert_eq!(
        expire(s.as_ref(), "other", T0 + HOUR, 1_000).await.unwrap(),
        2
    );
    let kept = request(s.as_ref(), me(), &k, OP, &hash('a'), "x", T0 + HOUR - 1).await;
    assert_eq!(kept.unwrap(), Outcome::Replayed("c0".into()));
}

pub async fn the_scope_comes_from_the_caller_and_never_from_the_request(fixture: Fixture) {
    let s = fixture.storage.clone();
    let k = key("origin");
    // Owner and worker sessions have no lease: empty run, epoch 0.
    for caller in [
        me(),
        Caller {
            project: P.into(),
            principal: Principal::Worker {
                identity: "w".into(),
            },
        },
    ] {
        let scope = RequestScope::of(&caller, &k).unwrap();
        assert_eq!((scope.run(), scope.epoch(), scope.project()), ("", 0, P));
    }
    // A task principal's run and epoch are its lease's.
    let scope = RequestScope::of(&task_principal(P, "r7", 3), &k).unwrap();
    assert_eq!(
        (scope.identity(), scope.run(), scope.epoch(), scope.key()),
        ("t", "r7", 3, k.as_str())
    );
    // A task principal cannot be given an owner's scope, nor an unusable one.
    for caller in [
        task_principal(P, "", 1),
        task_principal(P, "r1", 0),
        task_principal(P, "r1", u64::MAX),
        owner(P, ""),
        owner("", "owner"),
    ] {
        assert!(is_invalid(RequestScope::of(&caller, &k)), "{caller:?}");
    }
    // A scope belongs to the unit of work it was built for: another identity,
    // project, run or epoch is refused before anything is read or written.
    let mine = RequestScope::of(&me(), &k).unwrap();
    let theirs = [
        RequestScope::of(&owner(P, "someone-else"), &k).unwrap(),
        RequestScope::of(&owner("other", "owner"), &k).unwrap(),
        RequestScope::of(&task_principal(P, "r1", 1), &k).unwrap(),
    ];
    seed(s.as_ref(), P).await;
    s.transaction(me(), Lane::Entities, move |u| {
        for scope in &theirs {
            assert!(is_invalid(u.check_request_key(scope, OP, &hash('a'), T0)));
            assert!(is_invalid(u.store_request_key(
                scope,
                OP,
                &hash('a'),
                "x",
                T0
            )));
        }
        assert_eq!(
            u.check_request_key(&mine, OP, &hash('a'), T0)?,
            KeyCheck::New
        );
        Ok(())
    })
    .await
    .unwrap();
    // The refused stores wrote nothing under any scope.
    let task = task_principal(P, "r1", 1);
    let fresh = request(s.as_ref(), task, &k, OP, &hash('a'), "x", T0).await;
    assert_eq!(fresh.unwrap(), Outcome::Ran);
    // A task principal's own scope is used by that principal's unit of work.
    let owned = request(s.as_ref(), me(), &k, OP, &hash('a'), "x", T0).await;
    assert_eq!(owned.unwrap(), Outcome::Ran);
}

pub async fn input_outside_the_shapes_is_refused_and_changes_nothing(fixture: Fixture) {
    let s = fixture.storage.clone();
    seed(s.as_ref(), P).await;
    // Keys: 16 to 128 characters of A-Z a-z 0-9 _ -.
    for good in [
        "a".repeat(16),
        "Z".repeat(128),
        format!("{}-_9", "x".repeat(13)),
    ] {
        RequestScope::of(&me(), &good).unwrap();
    }
    for bad in [
        "a".repeat(15),
        "a".repeat(129),
        String::new(),
        format!("{}.", "a".repeat(16)),
        format!("{}/", "a".repeat(16)),
        format!("{} ", "a".repeat(16)),
        format!("{}é", "a".repeat(16)),
        format!("{}\n", "a".repeat(16)),
    ] {
        assert!(is_invalid(RequestScope::of(&me(), &bad)), "{bad:?}");
    }
    let k = key("shapes");
    s.transaction(me(), Lane::Entities, move |u| {
        let scope = RequestScope::of(u.caller(), &k)?;
        let long_op = "o".repeat(129);
        for op in ["", "has space", "a/b", "é", long_op.as_str()] {
            assert!(
                is_invalid(u.check_request_key(&scope, op, &hash('a'), T0)),
                "{op}"
            );
            assert!(is_invalid(u.store_request_key(
                &scope,
                op,
                &hash('a'),
                "x",
                T0
            )));
        }
        for req in [
            "",
            "abc",
            "A".repeat(64).as_str(),
            "g".repeat(64).as_str(),
            "a".repeat(65).as_str(),
        ] {
            assert!(
                is_invalid(u.check_request_key(&scope, OP, req, T0)),
                "{req}"
            );
            assert!(is_invalid(u.store_request_key(&scope, OP, req, "x", T0)));
        }
        // Time is UTC milliseconds from the epoch, and the expiry must fit.
        for now in [-1, i64::MIN, i64::MAX, i64::MAX - HOUR + 1] {
            assert!(
                is_invalid(u.store_request_key(&scope, OP, &hash('a'), "x", now)),
                "{now}"
            );
        }
        assert!(is_invalid(u.delete_expired_request_keys(-1, 10)));
        // A 128-byte operation and a clock at the last storable millisecond are fine.
        u.store_request_key(&scope, &"o".repeat(128), &hash('a'), "x", i64::MAX - HOUR)?;
        Ok(())
    })
    .await
    .unwrap();
    // Everything refused above left no row; only the last store exists.
    let after = request(s.as_ref(), me(), &key("shapes"), OP, &hash('a'), "x", T0).await;
    assert_eq!(after.unwrap(), Outcome::Conflict);
}

pub async fn a_key_is_replayable_for_exactly_one_hour(fixture: Fixture) {
    let s = fixture.storage.clone();
    seed(s.as_ref(), P).await;
    let k = key("ttl");
    request(s.as_ref(), me(), &k, OP, &hash('a'), "first", T0)
        .await
        .unwrap();
    for at in [T0, T0 + HOUR - 1] {
        let alive = request(s.as_ref(), me(), &k, OP, &hash('a'), "x", at).await;
        assert_eq!(alive.unwrap(), Outcome::Replayed("first".into()), "at {at}");
    }
    // At the boundary millisecond the key is already gone, though nothing has
    // deleted the row: a changed body is not a conflict and the change runs again.
    let reuse = request(s.as_ref(), me(), &k, OP, &hash('b'), "second", T0 + HOUR).await;
    assert_eq!(reuse.unwrap(), Outcome::Ran);
    assert_eq!(runs(s.as_ref(), P).await, 2);
    // The replacement has its own hour, from its own store.
    let again = request(s.as_ref(), me(), &k, OP, &hash('b'), "x", T0 + 2 * HOUR - 1).await;
    assert_eq!(again.unwrap(), Outcome::Replayed("second".into()));
    let gone = request(s.as_ref(), me(), &k, OP, &hash('b'), "third", T0 + 2 * HOUR).await;
    assert_eq!(gone.unwrap(), Outcome::Ran);
}

pub async fn a_result_over_64_kib_is_not_kept_and_says_so_on_replay(fixture: Fixture) {
    let s = fixture.storage.clone();
    seed(s.as_ref(), P).await;
    let exactly = "é".repeat(MAX_RESULT_BYTES / 2);
    assert_eq!(exactly.len(), MAX_RESULT_BYTES);
    let one_over = format!("{}x", exactly);
    // Bytes count, not characters: this has half as many characters as the bound.
    let wide = "é".repeat(MAX_RESULT_BYTES / 2 + 1);
    assert!(wide.chars().count() < MAX_RESULT_BYTES);
    for (name, result, kept) in [
        ("exact", exactly, true),
        ("over", one_over, false),
        ("wide", wide, false),
        ("empty", String::new(), true),
        ("huge", "x".repeat(1 << 20), false),
    ] {
        let k = key(name);
        let first = request(s.as_ref(), me(), &k, OP, &hash('a'), &result, T0).await;
        assert_eq!(first.unwrap(), Outcome::Ran, "{name}");
        let replay = request(s.as_ref(), me(), &k, OP, &hash('a'), "x", T0 + 1).await;
        let expected = if kept {
            Outcome::Replayed(result)
        } else {
            Outcome::ReplayedWithoutResult
        };
        assert_eq!(replay.unwrap(), expected, "{name}");
        // A truncated key still guards the change and still conflicts on a new body.
        let other = request(s.as_ref(), me(), &k, OP, &hash('b'), "x", T0 + 1).await;
        assert_eq!(other.unwrap(), Outcome::Conflict, "{name}");
    }
    assert_eq!(runs(s.as_ref(), P).await, 5);
}

pub async fn expiry_is_bounded_ordered_and_resumable(fixture: Fixture) {
    let s = fixture.storage.clone();
    seed(s.as_ref(), P).await;
    let total: i64 = 1_005;
    // Stored in an order unlike their expiry, one millisecond apart.
    s.transaction(me(), Lane::Entities, move |u| {
        for n in (0..total).rev() {
            let scope = RequestScope::of(u.caller(), &key(&format!("k{n:04}")))?;
            u.store_request_key(&scope, OP, &hash('a'), "x", T0 + n)?;
        }
        Ok(())
    })
    .await
    .unwrap();
    for bad in [0, 1_001, usize::MAX] {
        assert!(is_invalid(expire(s.as_ref(), P, T0 + 10 * HOUR, bad).await));
    }
    // Before the first expiry nothing is due.
    assert_eq!(
        expire(s.as_ref(), P, T0 + HOUR - 1, 1_000).await.unwrap(),
        0
    );
    // Rows with n up to 1,001 are due by now, 1,002 of them; one call takes 1,000.
    let now = T0 + HOUR + 1_001;
    assert_eq!(expire(s.as_ref(), P, now, 1_000).await.unwrap(), 1_000);
    // The earliest went first: nothing due at the thousandth row's own expiry is left.
    assert_eq!(
        expire(s.as_ref(), P, T0 + HOUR + 999, 1_000).await.unwrap(),
        0
    );
    assert_eq!(expire(s.as_ref(), P, now, 1_000).await.unwrap(), 2);
    assert_eq!(expire(s.as_ref(), P, now, 1_000).await.unwrap(), 0);
    // The three that were not due are still stored, and then due in their turn.
    let alive = request(s.as_ref(), me(), &key("k1004"), OP, &hash('a'), "x", now).await;
    assert_eq!(alive.unwrap(), Outcome::Replayed("x".into()));
    assert_eq!(
        expire(s.as_ref(), P, T0 + HOUR + 1_004, 1_000)
            .await
            .unwrap(),
        3
    );
}

pub async fn keys_survive_a_restore_and_stay_in_their_scope(fixture: Fixture) {
    let s = fixture.storage.clone();
    seed(s.as_ref(), P).await;
    let k = key("restore");
    for caller in [me(), task_principal(P, "r1", 1)] {
        request(s.as_ref(), caller, &k, OP, &hash('a'), "first", T0)
            .await
            .unwrap();
    }
    fixture.restore.restore_to_lineage("after-restore").await;
    // The rows stay and replay for the scope that wrote them.
    for caller in [me(), task_principal(P, "r1", 1)] {
        let again = request(s.as_ref(), caller, &k, OP, &hash('a'), "x", T0 + 1).await;
        assert_eq!(again.unwrap(), Outcome::Replayed("first".into()));
    }
    // A run after the restore holds a later epoch, so it finds nothing of the old one's.
    let fresh = request(
        s.as_ref(),
        task_principal(P, "r1", 2),
        &k,
        OP,
        &hash('a'),
        "new",
        T0 + 1,
    )
    .await;
    assert_eq!(fresh.unwrap(), Outcome::Ran);
}

macro_rules! request_key_suite {
    ($module:ident, $factory:path) => {
        mod $module {
            macro_rules! case {
                ($name:ident) => {
                    #[tokio::test]
                    async fn $name() {
                        crate::request_keys_v2::$name($factory().await).await;
                    }
                };
            }
            case!(a_repeated_change_runs_once);
            case!(the_same_key_with_another_operation_or_hash_is_a_mismatch);
            case!(a_store_over_a_live_key_is_refused_and_changes_nothing_even_if_caught);
            case!(key_and_change_commit_or_roll_back_together);
            case!(concurrent_repeats_run_the_change_once);
            case!(a_key_is_not_shared_across_identity_project_run_or_epoch);
            case!(the_scope_comes_from_the_caller_and_never_from_the_request);
            case!(input_outside_the_shapes_is_refused_and_changes_nothing);
            case!(a_key_is_replayable_for_exactly_one_hour);
            case!(a_result_over_64_kib_is_not_kept_and_says_so_on_replay);
            case!(expiry_is_bounded_ordered_and_resumable);
            case!(keys_survive_a_restore_and_stay_in_their_scope);
        }
    };
}
pub(crate) use request_key_suite;
