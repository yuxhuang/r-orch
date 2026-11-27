//! Task continuations through the owned public Storage entry point.
use orch_core::{
    clock::{Clock, iso_from_epoch_ms},
    store::{
        Caller, Cursor, Entity, EntityKind, Filter, Lane, PageRequest, Principal, Sort, Storage,
        StorageExt, StoreError,
    },
    test_support::make_task,
};
use orch_domain::TaskStatus;
use orch_infra::{
    data_dir::{DataDir, Holder, LockKind},
    sqlite::{SqliteStorage, StorageOptions},
};
use std::sync::{
    Arc,
    atomic::{AtomicI64, Ordering},
};

const SORTS: [Sort; 6] = [
    Sort::Default,
    Sort::Id,
    Sort::Position,
    Sort::Priority,
    Sort::CreatedAt,
    Sort::UpdatedAt,
];
const TTL: i64 = 86_400_000;
struct TestClock(AtomicI64);
impl Clock for TestClock {
    fn now_ms(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
    fn now_iso(&self) -> String {
        iso_from_epoch_ms(self.now_ms())
    }
}
fn caller(project: &str) -> Caller {
    Caller {
        project: project.into(),
        principal: Principal::Owner {
            identity: "synthetic".into(),
        },
    }
}
fn filter() -> Filter {
    Filter::Tasks {
        status: None,
        goal: None,
    }
}
fn request(sort: Sort, cursor: Option<Cursor>) -> PageRequest {
    PageRequest {
        sort,
        limit: 1,
        with_count: true,
        cursor,
    }
}
async fn fixture() -> (tempfile::TempDir, SqliteStorage, Arc<TestClock>) {
    let root = tempfile::tempdir().unwrap();
    let data = DataDir::acquire(
        &root.path().join("data"),
        &Holder {
            kind: LockKind::Daemon,
            pid: std::process::id(),
            started_at: "synthetic".into(),
            binary_version: "test".into(),
            executable: "/synthetic/orch".into(),
        },
    )
    .unwrap();
    let clock = Arc::new(TestClock(AtomicI64::new(1000)));
    let storage = SqliteStorage::open(
        Arc::new(data),
        StorageOptions {
            generation: "task-cursor-generation".into(),
            clock: clock.clone(),
            read_connections: 1,
            ..Default::default()
        },
    )
    .unwrap();
    for project in ["p", "other"] {
        storage
            .transaction(caller(project), Lane::Entities, |unit| {
                for id in ["c", "a", "b"] {
                    let mut task = make_task();
                    task.id = id.into();
                    task.title = format!("original-{id}");
                    task.status = TaskStatus::Todo;
                    task.created_at = "2026-01-01T00:00:00.000Z".into();
                    task.updated_at = task.created_at.clone();
                    unit.create(Entity::Task(Box::new(task)))?;
                }
                Ok(())
            })
            .await
            .unwrap();
    }
    (root, storage, clock)
}
fn invalid_cursor<T: std::fmt::Debug>(result: Result<T, StoreError>) {
    assert!(
        matches!(result, Err(StoreError::InvalidCursor)),
        "{result:?}"
    );
}

#[tokio::test]
async fn every_sort_issues_bound_tokens_and_recovers_after_rejection() {
    let (_root, storage, _clock) = fixture().await;
    for sort in SORTS {
        let snapshot = storage.read(caller("p")).await.unwrap();
        let first = snapshot
            .list(&filter(), &request(sort, None))
            .await
            .unwrap();
        let token = first.next_cursor.unwrap();
        assert!(token.as_str().len() <= 512);
        for (f, s) in [
            (
                filter(),
                if sort == Sort::Id {
                    Sort::Position
                } else {
                    Sort::Id
                },
            ),
            (
                Filter::Tasks {
                    status: Some(TaskStatus::Done),
                    goal: None,
                },
                sort,
            ),
            (
                Filter::Tasks {
                    status: None,
                    goal: Some("different".into()),
                },
                sort,
            ),
        ] {
            invalid_cursor(snapshot.list(&f, &request(s, Some(token.clone()))).await);
        }
        for bad in [
            "ab".into(),
            token.as_str()[1..].into(),
            format!("{}zz", token.as_str()),
        ] {
            invalid_cursor(
                snapshot
                    .list(&filter(), &request(sort, Some(Cursor::new(bad).unwrap())))
                    .await,
            );
        }
        // The wire schema fixes integer widths and the final UTF-8 ID. Invalid
        // scalar tags cannot be encoded; exercise truncated integers and invalid
        // text at the public boundary instead of inventing a different codec.
        let raw: Vec<u8> = token
            .as_str()
            .as_bytes()
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
            .collect();
        let keys = match sort {
            Sort::Default => 3,
            Sort::Id => 0,
            Sort::Position => 1,
            _ => 2,
        };
        let mut malformed = vec![raw[..25].to_vec()];
        if keys > 0 {
            malformed.push(raw[..26 + keys * 8 - 1].to_vec());
        }
        let mut bad_text = raw.clone();
        bad_text.push(0xff);
        malformed.push(bad_text);
        let mut bad_version = raw.clone();
        bad_version[0] = 255;
        malformed.push(bad_version);
        for bytes in malformed {
            let text: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
            invalid_cursor(
                snapshot
                    .list(&filter(), &request(sort, Some(Cursor::new(text).unwrap())))
                    .await,
            );
        }
        let second = snapshot
            .list(&filter(), &request(sort, Some(token.clone())))
            .await
            .unwrap();
        assert_eq!(second.total, Some(3));
        assert_ne!(first.items[0].entity.id(), second.items[0].entity.id());
        drop(snapshot);
        let other = storage.read(caller("other")).await.unwrap();
        invalid_cursor(other.list(&filter(), &request(sort, Some(token))).await);
    }
    assert!(storage.failure_state().failure().is_none());
    storage.close().await.unwrap();
}

#[tokio::test]
async fn invalid_attempt_and_limits_precede_an_id_continuation() {
    let (_root, storage, _clock) = fixture().await;
    let snapshot = storage.read(caller("p")).await.unwrap();
    let token = snapshot
        .list(&filter(), &request(Sort::Id, None))
        .await
        .unwrap()
        .next_cursor;
    for sort in [Sort::Attempt, Sort::Name] {
        assert!(matches!(
            snapshot
                .list(&filter(), &request(sort, token.clone()))
                .await,
            Err(StoreError::InvalidPage(_))
        ));
    }
    for limit in [0, 1001] {
        let req = PageRequest {
            limit,
            ..request(Sort::Id, Some(Cursor::new("ab".into()).unwrap()))
        };
        assert!(matches!(
            snapshot.list(&filter(), &req).await,
            Err(StoreError::InvalidPage(_))
        ));
    }
    assert!(
        snapshot
            .list(&filter(), &request(Sort::Id, token))
            .await
            .is_ok()
    );
    storage.close().await.unwrap();
}

#[tokio::test]
async fn tokens_expire_at_the_clock_boundary_and_refuse_future_claims() {
    let (_root, storage, clock) = fixture().await;
    let snapshot = storage.read(caller("p")).await.unwrap();
    for sort in SORTS {
        clock.0.store(1000, Ordering::SeqCst);
        let token = snapshot
            .list(&filter(), &request(sort, None))
            .await
            .unwrap()
            .next_cursor;
        clock.0.store(999, Ordering::SeqCst);
        invalid_cursor(
            snapshot
                .list(&filter(), &request(sort, token.clone()))
                .await,
        );
        clock.0.store(1000 + TTL, Ordering::SeqCst);
        assert!(
            snapshot
                .list(&filter(), &request(sort, token.clone()))
                .await
                .is_ok()
        );
        clock.0.store(1001 + TTL, Ordering::SeqCst);
        invalid_cursor(snapshot.list(&filter(), &request(sort, token)).await);
    }
    storage.close().await.unwrap();
}

#[tokio::test]
async fn every_sort_hydrates_and_continues_the_original_snapshot() {
    let (_root, storage, _clock) = fixture().await;
    for sort in SORTS {
        let snapshot = storage.read(caller("p")).await.unwrap();
        let whole = snapshot
            .list(
                &filter(),
                &PageRequest {
                    limit: 100,
                    ..request(sort, None)
                },
            )
            .await
            .unwrap();
        let first = snapshot
            .list(&filter(), &request(sort, None))
            .await
            .unwrap();
        let changed_id = whole.items[1].entity.id().to_owned();
        let deleted_id = changed_id.clone();
        storage
            .transaction(caller("p"), Lane::Entities, move |unit| {
                let record = unit.get(EntityKind::Task, &deleted_id)?.unwrap();
                unit.delete(EntityKind::Task, &deleted_id, record.revision)?;
                Ok(())
            })
            .await
            .unwrap();
        let mut items = first.items;
        let mut cursor = first.next_cursor;
        while let Some(token) = cursor {
            let page = snapshot
                .list(&filter(), &request(sort, Some(token)))
                .await
                .unwrap();
            assert_eq!(page.total, Some(whole.items.len() as u64));
            items.extend(page.items);
            cursor = page.next_cursor;
        }
        assert_eq!(items, whole.items);
        assert!(items.iter().any(|r| r.entity.id() == changed_id));
        drop(snapshot);
        // Restore the deleted row for the next sort without relying on its position.
        storage
            .transaction(caller("p"), Lane::Entities, move |unit| {
                let record = whole
                    .items
                    .iter()
                    .find(|r| r.entity.id() == changed_id)
                    .unwrap();
                unit.create(record.entity.clone()).map(|_| ())
            })
            .await
            .unwrap();
    }
    storage.close().await.unwrap();
}

#[tokio::test]
async fn a_new_generation_refuses_issued_tokens() {
    let (root, storage, _clock) = fixture().await;
    let snapshot = storage.read(caller("p")).await.unwrap();
    let mut tokens = Vec::new();
    for sort in SORTS {
        let token = snapshot
            .list(&filter(), &request(sort, None))
            .await
            .unwrap()
            .next_cursor
            .unwrap();
        tokens.push((sort, token));
    }
    drop(snapshot);
    storage.close().await.unwrap();
    drop(storage);
    let data = DataDir::acquire(
        &root.path().join("data"),
        &Holder {
            kind: LockKind::Daemon,
            pid: std::process::id(),
            started_at: "synthetic".into(),
            binary_version: "test".into(),
            executable: "/synthetic/orch".into(),
        },
    )
    .unwrap();
    let storage = SqliteStorage::open(
        Arc::new(data),
        StorageOptions {
            generation: "new-task-cursor-generation".into(),
            clock: Arc::new(TestClock(AtomicI64::new(1000))),
            ..Default::default()
        },
    )
    .unwrap();
    let snapshot = storage.read(caller("p")).await.unwrap();
    for (sort, token) in tokens {
        invalid_cursor(snapshot.list(&filter(), &request(sort, Some(token))).await);
        assert_eq!(
            snapshot
                .list(&filter(), &request(sort, None))
                .await
                .unwrap()
                .total,
            Some(3)
        );
    }
    invalid_cursor(Cursor::new(String::new()));
    invalid_cursor(Cursor::new("x".repeat(513)));
    storage.close().await.unwrap();
}
