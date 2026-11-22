//! Backend-neutral retry table scenarios: ordering, bounds, rollback,
//! isolation, and the fence of the lease operations that finish a run.
#![allow(
    clippy::unwrap_used,
    reason = "helpers outside #[test] functions; clippy.toml allows unwrap only inside them"
)]

use crate::v2_operations::Fixture;
use orch_core::store::{leases::*, retries::*, *};

const P: &str = "proj";

fn caller(project: &str) -> Caller {
    Caller {
        project: project.into(),
        principal: Principal::Owner {
            identity: "synthetic".into(),
        },
    }
}

fn retry(task: &str, attempt: u32, due_at: i64) -> Retry {
    Retry {
        task: task.into(),
        attempt,
        due_at,
        error: format!("{task} failed"),
    }
}

async fn put(s: &dyn Storage, project: &str, rows: Vec<Retry>) {
    s.transaction(caller(project), Lane::Entities, move |u| {
        for row in &rows {
            u.put_retry(row)?;
        }
        Ok(())
    })
    .await
    .unwrap();
}

async fn due(s: &dyn Storage, project: &str, now: i64, limit: usize) -> Vec<Retry> {
    s.read(caller(project))
        .await
        .unwrap()
        .read_due_retries(now, limit)
        .await
        .unwrap()
}

async fn get(s: &dyn Storage, project: &str, task: &str) -> Option<Retry> {
    s.read(caller(project))
        .await
        .unwrap()
        .read_retry(task)
        .await
        .unwrap()
}

fn tasks(rows: &[Retry]) -> Vec<&str> {
    rows.iter().map(|r| r.task.as_str()).collect()
}

pub async fn a_task_has_one_row_and_a_put_replaces_it_whole(fixture: Fixture) {
    let s = fixture.storage.clone();
    assert_eq!(get(s.as_ref(), P, "t1").await, None);
    put(s.as_ref(), P, vec![retry("t1", 1, 5_000)]).await;
    assert_eq!(get(s.as_ref(), P, "t1").await, Some(retry("t1", 1, 5_000)));
    let second = Retry {
        task: "t1".into(),
        attempt: 2,
        due_at: 9_000,
        error: String::new(),
    };
    put(s.as_ref(), P, vec![second.clone()]).await;
    assert_eq!(get(s.as_ref(), P, "t1").await, Some(second.clone()));
    assert_eq!(due(s.as_ref(), P, i64::MAX, 10).await, vec![second]);
}

pub async fn due_rows_are_ordered_by_deadline_then_task_and_bounded(fixture: Fixture) {
    let s = fixture.storage.clone();
    put(
        s.as_ref(),
        P,
        vec![
            retry("b", 1, 2_000),
            retry("a", 1, 2_000),
            retry("late", 1, 9_000),
            retry("early", 3, 1_000),
            retry("c", 1, 2_000),
        ],
    )
    .await;
    // The deadline itself is due; one millisecond short of it is not.
    assert_eq!(
        tasks(&due(s.as_ref(), P, 999, 10).await),
        Vec::<&str>::new()
    );
    assert_eq!(tasks(&due(s.as_ref(), P, 1_000, 10).await), ["early"]);
    assert_eq!(
        tasks(&due(s.as_ref(), P, 2_000, 10).await),
        ["early", "a", "b", "c"]
    );
    assert_eq!(
        tasks(&due(s.as_ref(), P, i64::MAX, 10).await),
        ["early", "a", "b", "c", "late"]
    );
    // A limit takes the earliest rows and leaves the rest where they are.
    assert_eq!(
        tasks(&due(s.as_ref(), P, i64::MAX, 2).await),
        ["early", "a"]
    );
    assert_eq!(due(s.as_ref(), P, i64::MAX, 10).await.len(), 5);
    // Replacing a row moves it by its new deadline, not by when it was written.
    put(s.as_ref(), P, vec![retry("early", 4, 8_000)]).await;
    assert_eq!(
        tasks(&due(s.as_ref(), P, i64::MAX, 10).await),
        ["a", "b", "c", "early", "late"]
    );
}

pub async fn the_limit_is_one_to_one_thousand(fixture: Fixture) {
    let s = fixture.storage.clone();
    let snap = s.read(caller(P)).await.unwrap();
    for bad in [0, MAX_DUE_RETRIES + 1] {
        assert!(matches!(
            snap.read_due_retries(0, bad).await,
            Err(StoreError::InvalidRetry(_))
        ));
        let result = s
            .transaction(caller(P), Lane::Entities, move |u| {
                u.due_retries(0, bad).map(|_| ())
            })
            .await;
        assert!(matches!(result, Err(StoreError::InvalidRetry(_))));
    }
    assert!(snap.read_due_retries(0, 1).await.unwrap().is_empty());
    assert!(
        snap.read_due_retries(0, MAX_DUE_RETRIES)
            .await
            .unwrap()
            .is_empty()
    );
}

pub async fn no_row_is_dropped_for_lack_of_room(fixture: Fixture) {
    let s = fixture.storage.clone();
    // 1.0 kept 100 and dropped the oldest; there is no cap here.
    let rows: Vec<_> = (0..1_200)
        .map(|i| retry(&format!("t{i:04}"), 1, i64::from(i)))
        .collect();
    put(s.as_ref(), P, rows).await;
    assert_eq!(
        get(s.as_ref(), P, "t0000").await,
        Some(retry("t0000", 1, 0))
    );
    let first = due(s.as_ref(), P, i64::MAX, MAX_DUE_RETRIES).await;
    assert_eq!(first.len(), 1_000);
    assert_eq!(first[0].task, "t0000");
    assert_eq!(first[999].task, "t0999");
    let doomed: Vec<_> = first.iter().map(|r| r.task.clone()).collect();
    s.transaction(caller(P), Lane::Entities, move |u| {
        for task in &doomed {
            assert!(u.delete_retry(task)?);
        }
        Ok(())
    })
    .await
    .unwrap();
    let rest = due(s.as_ref(), P, i64::MAX, MAX_DUE_RETRIES).await;
    assert_eq!(rest.len(), 200);
    assert_eq!(rest[0].task, "t1000");
    assert_eq!(rest[199].task, "t1199");
}

pub async fn delete_reports_whether_a_row_existed_and_is_repeatable(fixture: Fixture) {
    let s = fixture.storage.clone();
    put(s.as_ref(), P, vec![retry("t1", 1, 1), retry("t2", 1, 2)]).await;
    let seen = s
        .transaction(caller(P), Lane::Entities, |u| {
            Ok([
                u.delete_retry("t1")?,
                u.delete_retry("t1")?,
                u.delete_retry("never")?,
            ])
        })
        .await
        .unwrap();
    assert_eq!(seen, [true, false, false]);
    assert_eq!(get(s.as_ref(), P, "t1").await, None);
    assert_eq!(get(s.as_ref(), P, "t2").await, Some(retry("t2", 1, 2)));
}

pub async fn a_failed_unit_of_work_changes_no_retry_row(fixture: Fixture) {
    let s = fixture.storage.clone();
    put(
        s.as_ref(),
        P,
        vec![retry("keep", 1, 10), retry("gone", 1, 20)],
    )
    .await;
    let result = s
        .transaction(caller(P), Lane::Entities, |u| {
            u.create(Entity::Task(Box::new(crate::contract::task("new"))))?;
            u.put_retry(&retry("keep", 9, 99))?;
            u.put_retry(&retry("added", 1, 1))?;
            assert!(u.delete_retry("gone")?);
            assert_eq!(u.get_retry("keep")?, Some(retry("keep", 9, 99)));
            Err::<(), _>(StoreError::CounterExhausted)
        })
        .await;
    assert!(matches!(result, Err(StoreError::CounterExhausted)));
    let view = s.read(caller(P)).await.unwrap();
    assert!(view.get(EntityKind::Task, "new").await.unwrap().is_none());
    assert_eq!(
        tasks(&view.read_due_retries(i64::MAX, 10).await.unwrap()),
        ["keep", "gone"]
    );
    assert_eq!(
        view.read_retry("keep").await.unwrap(),
        Some(retry("keep", 1, 10))
    );
    assert_eq!(
        view.read_retry("gone").await.unwrap(),
        Some(retry("gone", 1, 20))
    );
    assert_eq!(view.read_retry("added").await.unwrap(), None);
}

pub async fn a_refused_row_changes_nothing_even_when_the_caller_commits(fixture: Fixture) {
    let s = fixture.storage.clone();
    put(s.as_ref(), P, vec![retry("t1", 1, 10)]).await;
    let mut long = retry("t1", 2, 20);
    long.error = "x".repeat(MAX_RETRY_ERROR_BYTES + 1);
    let mut exact = retry("t2", 1, 30);
    exact.error = "y".repeat(MAX_RETRY_ERROR_BYTES);
    let bad = vec![
        retry("", 1, 1),
        retry("t1", 0, 20),
        retry("t1", 2, -1),
        long,
    ];
    let exact_row = exact.clone();
    s.transaction(caller(P), Lane::Entities, move |u| {
        for row in &bad {
            assert!(matches!(u.put_retry(row), Err(StoreError::InvalidRetry(_))));
        }
        // The boundary itself is accepted, and the commit keeps only that.
        u.put_retry(&exact_row)
    })
    .await
    .unwrap();
    assert_eq!(get(s.as_ref(), P, "t1").await, Some(retry("t1", 1, 10)));
    assert_eq!(get(s.as_ref(), P, "t2").await, Some(exact));
    assert_eq!(due(s.as_ref(), P, i64::MAX, 10).await.len(), 2);
}

pub async fn a_snapshot_keeps_its_rows_and_a_unit_of_work_reads_its_own_writes(fixture: Fixture) {
    let s = fixture.storage.clone();
    put(s.as_ref(), P, vec![retry("t1", 1, 10)]).await;
    let before = s.read(caller(P)).await.unwrap();
    s.transaction(caller(P), Lane::Entities, |u| {
        u.put_retry(&retry("t2", 1, 5))?;
        assert!(u.delete_retry("t1")?);
        assert_eq!(u.get_retry("t1")?, None);
        assert_eq!(tasks(&u.due_retries(i64::MAX, 10)?), ["t2"]);
        Ok(())
    })
    .await
    .unwrap();
    assert_eq!(
        tasks(&before.read_due_retries(i64::MAX, 10).await.unwrap()),
        ["t1"]
    );
    assert_eq!(before.read_retry("t2").await.unwrap(), None);
    assert_eq!(tasks(&due(s.as_ref(), P, i64::MAX, 10).await), ["t2"]);
}

pub async fn projects_do_not_see_or_change_each_others_rows(fixture: Fixture) {
    let s = fixture.storage.clone();
    put(s.as_ref(), P, vec![retry("t1", 1, 10)]).await;
    put(
        s.as_ref(),
        "other",
        vec![retry("t1", 7, 70), retry("t9", 1, 1)],
    )
    .await;
    s.transaction(caller("other"), Lane::Entities, |u| {
        assert!(u.delete_retry("t1")?);
        Ok(())
    })
    .await
    .unwrap();
    assert_eq!(get(s.as_ref(), P, "t1").await, Some(retry("t1", 1, 10)));
    assert_eq!(get(s.as_ref(), "other", "t1").await, None);
    assert_eq!(tasks(&due(s.as_ref(), "other", i64::MAX, 10).await), ["t9"]);
    assert_eq!(
        tasks(&due(s.as_ref(), "never-written", i64::MAX, 10).await),
        Vec::<&str>::new()
    );
}

pub async fn retry_rows_commit_or_roll_back_with_the_lease_that_ends_a_run(fixture: Fixture) {
    let s = fixture.storage.clone();
    let lease = s
        .transaction(caller(P), Lane::Claims, |u| {
            let ClaimOutcome::Granted(lease) = u.claim_lease(
                &NewLease {
                    task: "t1".into(),
                    agent: "a1".into(),
                    run: "r1".into(),
                },
                0,
            )?
            else {
                panic!("fresh claim blocked");
            };
            Ok(lease)
        })
        .await
        .unwrap();
    // A finish that fails its fence takes the retry row it wrote with it.
    let stale = lease.epoch + 1;
    let token = lease.clone();
    let result = s
        .transaction(caller(P), Lane::Entities, move |u| {
            u.put_retry(&retry("t1", 1, 10))?;
            u.release_lease("t1", &token.lineage, stale)?;
            Ok(())
        })
        .await;
    assert!(matches!(result, Err(StoreError::StaleEpoch)));
    assert_eq!(get(s.as_ref(), P, "t1").await, None);
    assert_eq!(
        s.read(caller(P))
            .await
            .unwrap()
            .read_leases()
            .await
            .unwrap(),
        vec![lease.clone()]
    );
    // The finish that passes commits the row and the release together, and the
    // next start removes the row with its claim.
    let token = lease.clone();
    s.transaction(caller(P), Lane::Entities, move |u| {
        u.put_retry(&retry("t1", 1, 10))?;
        u.release_lease("t1", &token.lineage, token.epoch)?;
        Ok(())
    })
    .await
    .unwrap();
    assert_eq!(get(s.as_ref(), P, "t1").await, Some(retry("t1", 1, 10)));
    assert!(
        s.read(caller(P))
            .await
            .unwrap()
            .read_leases()
            .await
            .unwrap()
            .is_empty()
    );
    s.transaction(caller(P), Lane::Claims, |u| {
        let ClaimOutcome::Granted(next) = u.claim_lease(
            &NewLease {
                task: "t1".into(),
                agent: "a1".into(),
                run: "r2".into(),
            },
            20,
        )?
        else {
            panic!("claim blocked");
        };
        assert_eq!(next.epoch, 2);
        assert!(u.delete_retry("t1")?);
        Ok(())
    })
    .await
    .unwrap();
    assert_eq!(get(s.as_ref(), P, "t1").await, None);
}

pub async fn a_restore_keeps_every_retry_row(fixture: Fixture) {
    let s = fixture.storage.clone();
    put(s.as_ref(), P, vec![retry("t1", 3, 10), retry("t2", 1, 5)]).await;
    fixture.restore.restore_to_lineage("restored-lineage").await;
    assert_eq!(
        due(s.as_ref(), P, i64::MAX, 10).await,
        vec![retry("t2", 1, 5), retry("t1", 3, 10)]
    );
}

macro_rules! retry_suite {
    ($module:ident, $factory:path) => {
        mod $module {
            macro_rules! case {
                ($name:ident) => {
                    #[tokio::test]
                    async fn $name() {
                        crate::retries_v2::$name($factory().await).await;
                    }
                };
            }
            case!(a_task_has_one_row_and_a_put_replaces_it_whole);
            case!(due_rows_are_ordered_by_deadline_then_task_and_bounded);
            case!(the_limit_is_one_to_one_thousand);
            case!(no_row_is_dropped_for_lack_of_room);
            case!(delete_reports_whether_a_row_existed_and_is_repeatable);
            case!(a_failed_unit_of_work_changes_no_retry_row);
            case!(a_refused_row_changes_nothing_even_when_the_caller_commits);
            case!(a_snapshot_keeps_its_rows_and_a_unit_of_work_reads_its_own_writes);
            case!(projects_do_not_see_or_change_each_others_rows);
            case!(retry_rows_commit_or_roll_back_with_the_lease_that_ends_a_run);
            case!(a_restore_keeps_every_retry_row);
        }
    };
}
pub(crate) use retry_suite;
