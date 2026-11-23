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

/// Walks the whole complete read of one snapshot with `limit` rows a page,
/// returning the pages' sizes and every row in the order they arrived.
async fn walk(snap: &dyn ReadSnapshot, limit: usize) -> (Vec<usize>, Vec<Retry>) {
    let (mut sizes, mut rows, mut after) = (Vec::new(), Vec::new(), None);
    loop {
        let page = snap.read_retries(after.as_ref(), limit).await.unwrap();
        assert!(!page.rows.is_empty() || (rows.is_empty() && page.next.is_none()));
        sizes.push(page.rows.len());
        assert!(sizes.len() <= 3_000, "the cursor does not advance");
        rows.extend(page.rows);
        match page.next {
            Some(next) => after = Some(next),
            None => return (sizes, rows),
        }
    }
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
    let mut exact = retry("t2", 1, 30);
    exact.error = "y".repeat(MAX_RETRY_ERROR_BYTES);
    let bad = vec![retry("", 1, 1), retry("t1", 0, 20), retry("t1", 2, -1)];
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

/// 2,500 rows, seven to a deadline so that ties are everywhere, written in
/// descending id order so that no backend can return them in insertion order.
fn many_rows() -> Vec<Retry> {
    (0..2_500)
        .rev()
        .map(|i| retry(&format!("t{i:04}"), 1, 1_000 + i64::from(i / 7) * 10))
        .collect()
}

pub async fn the_complete_read_pages_every_row_including_future_ones_in_read_order(
    fixture: Fixture,
) {
    let s = fixture.storage.clone();
    assert!(
        s.read(caller(P))
            .await
            .unwrap()
            .read_retries(None, 10)
            .await
            .unwrap()
            == RetryPage {
                rows: Vec::new(),
                next: None
            }
    );
    put(s.as_ref(), P, many_rows()).await;
    let mut expected = many_rows();
    expected.sort_by(|a, b| (a.due_at, &a.task).cmp(&(b.due_at, &b.task)));
    let snap = s.read(caller(P)).await.unwrap();
    // Nothing is due yet, and every row is still read.
    assert!(snap.read_due_retries(999, 10).await.unwrap().is_empty());
    let (sizes, rows) = walk(snap.as_ref(), MAX_DUE_RETRIES).await;
    assert_eq!(sizes, [1_000, 1_000, 500]);
    assert_eq!(rows, expected);
    // Other page sizes cover the same rows once each; a total that is an exact
    // multiple of the limit ends on a full page and no empty one.
    let (sizes, rows) = walk(snap.as_ref(), 500).await;
    assert_eq!(sizes, [500; 5]);
    assert_eq!(rows, expected);
    let (sizes, rows) = walk(snap.as_ref(), 999).await;
    assert_eq!(sizes, [999, 999, 502]);
    assert_eq!(rows, expected);
    let (sizes, rows) = walk(snap.as_ref(), 1).await;
    assert_eq!((sizes.len(), rows), (2_500, expected.clone()));
    // The due read is the prefix of the complete read it filters (here with
    // fewer than the due read's own bound due), and the rest is still ahead.
    let now = expected[499].due_at;
    let due_now = snap.read_due_retries(now, MAX_DUE_RETRIES).await.unwrap();
    assert!(!due_now.is_empty() && due_now.len() < expected.len());
    assert_eq!(due_now[..], expected[..due_now.len()]);
    assert!(expected[due_now.len()..].iter().all(|r| r.due_at > now));
}

pub async fn ties_and_task_ids_are_read_in_one_stable_byte_order(fixture: Fixture) {
    let s = fixture.storage.clone();
    // One deadline, ids that differ by case, digits and a non-ASCII letter:
    // the order is bytewise, not locale or case folding.
    put(
        s.as_ref(),
        P,
        ["é", "b", "a", "Z", "10", "9", "B", "a1"]
            .map(|t| retry(t, 1, 500))
            .into(),
    )
    .await;
    put(s.as_ref(), P, vec![retry("zz", 1, 100)]).await;
    let snap = s.read(caller(P)).await.unwrap();
    let want = ["zz", "10", "9", "B", "Z", "a", "a1", "b", "é"];
    for limit in [1, 2, 3, 100] {
        let (_, rows) = walk(snap.as_ref(), limit).await;
        assert_eq!(tasks(&rows), want, "limit {limit}");
    }
    // The same table read twice gives the same pages.
    let first = snap.read_retries(None, 4).await.unwrap();
    assert_eq!(first, snap.read_retries(None, 4).await.unwrap());
    // A cursor between two rows of one deadline continues at the next task id.
    let cursor = RetryCursor {
        due_at: 500,
        task: "a".into(),
    };
    let page = snap.read_retries(Some(&cursor), 100).await.unwrap();
    assert_eq!((tasks(&page.rows), page.next), (vec!["a1", "b", "é"], None));
}

pub async fn the_complete_read_limit_and_cursor_are_checked(fixture: Fixture) {
    let s = fixture.storage.clone();
    put(s.as_ref(), P, vec![retry("t1", 1, 10)]).await;
    let snap = s.read(caller(P)).await.unwrap();
    let bad_cursors = [
        RetryCursor {
            due_at: 0,
            task: String::new(),
        },
        RetryCursor {
            due_at: -1,
            task: "t1".into(),
        },
    ];
    for bad in [0, MAX_DUE_RETRIES + 1] {
        assert!(matches!(
            snap.read_retries(None, bad).await,
            Err(StoreError::InvalidRetry(_))
        ));
        let result = s
            .transaction(caller(P), Lane::Entities, move |u| {
                u.list_retries(None, bad).map(|_| ())
            })
            .await;
        assert!(matches!(result, Err(StoreError::InvalidRetry(_))));
    }
    for bad in bad_cursors {
        assert!(matches!(
            snap.read_retries(Some(&bad), 0).await,
            Err(StoreError::InvalidRetry(_))
        ));
        let invalid = bad.clone();
        let result = s
            .transaction(caller(P), Lane::Entities, move |u| {
                u.list_retries(Some(&invalid), 0).map(|_| ())
            })
            .await;
        assert!(matches!(result, Err(StoreError::InvalidRetry(_))));
        assert!(matches!(
            snap.read_retries(Some(&bad), 10).await,
            Err(StoreError::InvalidCursor)
        ));
        let result = s
            .transaction(caller(P), Lane::Entities, move |u| {
                u.list_retries(Some(&bad), 10).map(|_| ())
            })
            .await;
        assert!(matches!(result, Err(StoreError::InvalidCursor)));
    }
    // Both bounds are accepted, and a cursor past the last row is an empty end.
    assert_eq!(snap.read_retries(None, 1).await.unwrap().rows.len(), 1);
    assert_eq!(
        snap.read_retries(None, MAX_DUE_RETRIES)
            .await
            .unwrap()
            .rows
            .len(),
        1
    );
    let past = RetryCursor {
        due_at: i64::MAX,
        task: "t1".into(),
    };
    assert_eq!(
        snap.read_retries(Some(&past), 10).await.unwrap(),
        RetryPage {
            rows: Vec::new(),
            next: None
        }
    );
}

pub async fn a_snapshot_walk_ignores_commits_made_between_its_pages(fixture: Fixture) {
    let s = fixture.storage.clone();
    put(s.as_ref(), P, many_rows()).await;
    let mut expected = many_rows();
    expected.sort_by(|a, b| (a.due_at, &a.task).cmp(&(b.due_at, &b.task)));
    let snap = s.read(caller(P)).await.unwrap();
    let first = snap.read_retries(None, 1_000).await.unwrap();
    let cursor = first.next.clone().unwrap();
    assert_eq!(cursor, RetryCursor::after(&first.rows[999]));
    // Between pages: delete the cursor's own row and two later ones, replace a
    // later row and an earlier one, and add rows on both sides of the cursor.
    let (gone, moved) = (first.rows[999].task.clone(), expected[1_500].task.clone());
    let (gone2, early) = (expected[2_000].task.clone(), first.rows[3].task.clone());
    s.transaction(caller(P), Lane::Entities, move |u| {
        assert!(u.delete_retry(&gone)?);
        assert!(u.delete_retry(&gone2)?);
        u.put_retry(&retry(&moved, 2, 0))?;
        u.put_retry(&retry(&early, 2, 9_999_999))?;
        u.put_retry(&retry("before-everything", 1, 0))?;
        u.put_retry(&retry("zz-after-everything", 1, i64::MAX))?;
        Ok(())
    })
    .await
    .unwrap();
    // The old snapshot's remaining pages are the original table's.
    let mut rows = first.rows;
    let mut after = Some(cursor);
    let mut pages = 0;
    while let Some(cursor) = after {
        pages += 1;
        assert!(pages <= 3, "the cursor does not advance");
        let page = snap.read_retries(Some(&cursor), 1_000).await.unwrap();
        rows.extend(page.rows);
        after = page.next;
    }
    assert_eq!(rows, expected);
    // A new snapshot reads the new table, whole and in order.
    let fresh = s.read(caller(P)).await.unwrap();
    let (_, now) = walk(fresh.as_ref(), 1_000).await;
    assert_eq!(now.len(), 2_500 - 2 + 2);
    assert_eq!(now[0].task, "before-everything");
    assert_eq!(now.last().unwrap().task, "zz-after-everything");
    assert!(
        now.windows(2)
            .all(|w| (w[0].due_at, &w[0].task) < (w[1].due_at, &w[1].task))
    );
}

pub async fn a_cursor_outlives_its_row_in_a_later_snapshot(fixture: Fixture) {
    let s = fixture.storage.clone();
    put(
        s.as_ref(),
        P,
        vec![retry("a", 1, 10), retry("b", 1, 20), retry("c", 1, 30)],
    )
    .await;
    let page = s
        .read(caller(P))
        .await
        .unwrap()
        .read_retries(None, 2)
        .await
        .unwrap();
    let cursor = page.next.unwrap();
    assert_eq!(cursor, RetryCursor::after(&retry("b", 1, 20)));
    // The row the cursor names is replaced with a later deadline: the walk
    // continues after the old key, and the replaced row is read again later.
    put(s.as_ref(), P, vec![retry("b", 2, 40)]).await;
    let snap = s.read(caller(P)).await.unwrap();
    let rest = snap.read_retries(Some(&cursor), 10).await.unwrap();
    assert_eq!(tasks(&rest.rows), ["c", "b"]);
    // And deleted outright it just continues.
    s.transaction(caller(P), Lane::Entities, |u| {
        u.delete_retry("b")?;
        Ok(())
    })
    .await
    .unwrap();
    let snap = s.read(caller(P)).await.unwrap();
    let rest = snap.read_retries(Some(&cursor), 10).await.unwrap();
    assert_eq!(tasks(&rest.rows), ["c"]);
}

pub async fn a_unit_of_work_pages_its_own_writes(fixture: Fixture) {
    let s = fixture.storage.clone();
    put(
        s.as_ref(),
        P,
        vec![retry("t1", 1, 10), retry("t2", 1, 20), retry("t3", 1, 30)],
    )
    .await;
    s.transaction(caller(P), Lane::Entities, |u| {
        u.put_retry(&retry("t0", 1, 5))?;
        assert!(u.delete_retry("t2")?);
        u.put_retry(&retry("t4", 1, i64::MAX))?;
        let first = u.list_retries(None, 2)?;
        assert_eq!(tasks(&first.rows), ["t0", "t1"]);
        let rest = u.list_retries(first.next.as_ref(), 2)?;
        assert_eq!(tasks(&rest.rows), ["t3", "t4"]);
        assert_eq!(rest.next, None);
        // A write between pages is seen by the next one.
        u.put_retry(&retry("t11", 1, 11))?;
        let rest = u.list_retries(first.next.as_ref(), 10)?;
        assert_eq!(tasks(&rest.rows), ["t11", "t3", "t4"]);
        Err::<(), _>(StoreError::CounterExhausted)
    })
    .await
    .unwrap_err();
    // Nothing of that stayed.
    let (_, rows) = walk(s.read(caller(P)).await.unwrap().as_ref(), 10).await;
    assert_eq!(tasks(&rows), ["t1", "t2", "t3"]);
}

pub async fn the_complete_read_is_one_projects_rows_only(fixture: Fixture) {
    let s = fixture.storage.clone();
    put(s.as_ref(), P, vec![retry("t1", 1, 10), retry("t3", 1, 30)]).await;
    put(
        s.as_ref(),
        "other",
        vec![retry("t1", 7, 70), retry("t2", 1, 20), retry("t9", 1, 90)],
    )
    .await;
    let mine = s.read(caller(P)).await.unwrap();
    let (_, rows) = walk(mine.as_ref(), 1).await;
    assert_eq!(rows, vec![retry("t1", 1, 10), retry("t3", 1, 30)]);
    let theirs = s.read(caller("other")).await.unwrap();
    let (_, rows) = walk(theirs.as_ref(), 2).await;
    assert_eq!(
        tasks(&rows),
        ["t2", "t1", "t9"],
        "own rows, by their own deadlines"
    );
    assert_eq!(rows[1], retry("t1", 7, 70));
    // A cursor taken in one project reads only the other's own rows after it.
    let cursor = mine.read_retries(None, 1).await.unwrap().next.unwrap();
    assert_eq!(cursor.task, "t1");
    let page = theirs.read_retries(Some(&cursor), 10).await.unwrap();
    assert_eq!(tasks(&page.rows), ["t2", "t1", "t9"]);
    let never = s.read(caller("never-written")).await.unwrap();
    assert!(never.read_retries(None, 10).await.unwrap().rows.is_empty());
}

pub async fn a_restore_keeps_every_retry_row(fixture: Fixture) {
    let s = fixture.storage.clone();
    let mut expected = many_rows();
    expected.push(Retry {
        error: "é".repeat(MAX_RETRY_ERROR_BYTES / 2),
        ..retry("whole-error", 3, 0)
    });
    expected.sort_by(|a, b| (a.due_at, &a.task).cmp(&(b.due_at, &b.task)));
    put(s.as_ref(), P, expected.clone()).await;
    fixture.restore.restore_to_lineage("restored-lineage").await;
    let (pages, rows) = walk(s.read(caller(P)).await.unwrap().as_ref(), MAX_DUE_RETRIES).await;
    assert_eq!(pages, vec![1_000, 1_000, 501]);
    assert_eq!(rows, expected);
    // Enumeration is read-only: another complete walk returns the same text and rows.
    let (_, again) = walk(s.read(caller(P)).await.unwrap().as_ref(), 999).await;
    assert_eq!(again, expected);
}

// No run or task entity exists: the retry owns the complete diagnostic.
pub async fn retry_whole_text_round_trips_at_the_utf8_boundary_without_a_run(fixture: Fixture) {
    let s = fixture.storage.clone();
    let mut inline = retry("inline", 1, 10);
    inline.error = "é".repeat(32_768);
    let mut external = retry("external", 2, 20);
    external.error = format!("{}!", "é".repeat(32_768));
    assert_eq!(inline.error.len(), 65_536);
    assert_eq!(external.error.len(), 65_537);
    let expected = vec![inline, external];
    let rows = expected.clone();
    s.transaction(caller(P), Lane::Entities, move |u| {
        for row in &rows {
            assert!(u.get_retry(&row.task)?.is_none());
            u.put_retry(row)?;
            // T12 enqueue is get-before-put; a repeated enqueue keeps the first.
            if u.get_retry(&row.task)?.is_none() {
                u.put_retry(&retry(&row.task, 9, 99))?;
            }
            assert_eq!(u.get_retry(&row.task)?, Some(row.clone()));
        }
        assert_eq!(u.due_retries(i64::MAX, 10)?, rows);
        assert_eq!(u.list_retries(None, 10)?.rows, rows);
        Ok(())
    })
    .await
    .unwrap();
    let snapshot = s.read(caller(P)).await.unwrap();
    for row in &expected {
        assert_eq!(
            snapshot.read_retry(&row.task).await.unwrap(),
            Some(row.clone())
        );
    }
    assert_eq!(
        snapshot.read_due_retries(i64::MAX, 10).await.unwrap(),
        expected
    );
    assert_eq!(walk(snapshot.as_ref(), 1).await.1, expected);
    fixture
        .restore
        .restore_to_lineage("whole-text-restore")
        .await;
    assert_eq!(
        walk(s.read(caller(P)).await.unwrap().as_ref(), 1).await.1,
        expected
    );
}

pub async fn retry_whole_text_replacement_rolls_back_and_snapshots_keep_original(fixture: Fixture) {
    let s = fixture.storage.clone();
    let mut original = retry("t1", 1, 10);
    original.error = format!("{}!", "é".repeat(32_768));
    put(s.as_ref(), P, vec![original.clone()]).await;
    let snapshot = s.read(caller(P)).await.unwrap();
    let mut replacement = retry("t1", 2, 20);
    replacement.error = "new diagnostic".repeat(10_000);
    let new = replacement.clone();
    let result = s
        .transaction(caller(P), Lane::Entities, move |u| {
            u.put_retry(&new)?;
            u.put_retry(&Retry {
                task: "new".into(),
                ..new.clone()
            })?;
            assert_eq!(u.get_retry("t1")?, Some(new));
            Err::<(), _>(StoreError::CounterExhausted)
        })
        .await;
    assert!(matches!(result, Err(StoreError::CounterExhausted)));
    assert_eq!(get(s.as_ref(), P, "t1").await, Some(original.clone()));
    assert_eq!(get(s.as_ref(), P, "new").await, None);
    put(s.as_ref(), P, vec![replacement.clone()]).await;
    assert_eq!(get(s.as_ref(), P, "t1").await, Some(replacement));
    assert_eq!(
        snapshot.read_retry("t1").await.unwrap(),
        Some(original.clone())
    );
    assert_eq!(
        snapshot.read_due_retries(i64::MAX, 10).await.unwrap(),
        [original.clone()]
    );
    assert_eq!(walk(snapshot.as_ref(), 1).await.1, [original]);
    let short = retry("t1", 3, 30);
    put(s.as_ref(), P, vec![short.clone()]).await;
    assert_eq!(get(s.as_ref(), P, "t1").await, Some(short.clone()));
    let snap = s.read(caller(P)).await.unwrap();
    assert_eq!(
        snap.read_due_retries(i64::MAX, 1).await.unwrap().as_slice(),
        std::slice::from_ref(&short)
    );
    assert_eq!(walk(snap.as_ref(), 1).await.1, [short]);
    let mut long = retry("t1", 4, 40);
    long.error = "long".repeat(20_000);
    put(s.as_ref(), P, vec![long.clone()]).await;
    assert_eq!(get(s.as_ref(), P, "t1").await, Some(long));
}

pub async fn retry_chunked_text_survives_restore_without_a_run(fixture: Fixture) {
    let s = fixture.storage.clone();
    let mut row = retry("chunked", 1, 10);
    row.error = "x".repeat(64 * 1024 * 1024 + 1);
    put(s.as_ref(), P, vec![row.clone()]).await;
    fixture.restore.restore_to_lineage("chunked-restore").await;
    let snap = s.read(caller(P)).await.unwrap();
    assert_eq!(snap.read_retry("chunked").await.unwrap(), Some(row.clone()));
    assert_eq!(
        snap.read_due_retries(i64::MAX, 1).await.unwrap(),
        [row.clone()]
    );
    assert_eq!(walk(snap.as_ref(), 1).await.1, [row]);
}

struct BlobMemoryFixture {
    _root: tempfile::TempDir,
    storage: std::sync::Arc<orch_core::store::fake::FakeStorage>,
}

#[async_trait::async_trait]
impl crate::v2_operations::Restore for BlobMemoryFixture {
    async fn restore_to_lineage(&self, lineage: &str) {
        self.storage.restore_to_lineage(lineage).await;
    }
}

pub async fn blob_memory_backend() -> Fixture {
    use orch_infra::blob::{BlobHash, DiskBlobStore, MAX_BLOB_BYTES};
    use orch_infra::data_dir::confined::ConfinedDir;
    let root = tempfile::tempdir().unwrap();
    orch_core::test_support::assert_hermetic(root.path());
    let blobs = DiskBlobStore::open(&ConfinedDir::open(root.path()).unwrap()).unwrap();
    let storage = std::sync::Arc::new(orch_core::store::fake::FakeStorage::with_retry_blob_store(
        blobs,
        BlobHash::of,
        MAX_BLOB_BYTES as usize,
    ));
    storage.restore_to_lineage("initial-lineage").await;
    Fixture {
        storage: storage.clone(),
        restore: std::sync::Arc::new(BlobMemoryFixture {
            _root: root,
            storage,
        }),
        initial_lineage: "initial-lineage".into(),
    }
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
            case!(retry_chunked_text_survives_restore_without_a_run);
            case!(retry_whole_text_round_trips_at_the_utf8_boundary_without_a_run);
            case!(retry_whole_text_replacement_rolls_back_and_snapshots_keep_original);
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
            case!(the_complete_read_pages_every_row_including_future_ones_in_read_order);
            case!(ties_and_task_ids_are_read_in_one_stable_byte_order);
            case!(the_complete_read_limit_and_cursor_are_checked);
            case!(a_snapshot_walk_ignores_commits_made_between_its_pages);
            case!(a_cursor_outlives_its_row_in_a_later_snapshot);
            case!(a_unit_of_work_pages_its_own_writes);
            case!(the_complete_read_is_one_projects_rows_only);
            case!(a_restore_keeps_every_retry_row);
        }
    };
}
pub(crate) use retry_suite;
