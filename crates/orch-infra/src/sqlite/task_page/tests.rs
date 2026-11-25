use super::{self as task_page, Continuation, query};
use crate::data_dir::{DataDir, Holder, LockKind};
use crate::sqlite::{ConnectionRole, Synchronous, migrations::migrate, open_connection};
use crate::sqlite::{ReadPool, SqliteError};
use crate::storage::v2::FileStorage;
use orch_core::store::{
    Caller, Entity, EntityKind, Filter, Lane, PageRequest, Principal, Revision, Sort, Storage,
    StoreError,
};
use orch_core::test_support::make_task;
use orch_domain::{Task, TaskStatus};
use std::time::{Duration, Instant};

fn owner(project: &str) -> Caller {
    Caller {
        project: project.into(),
        principal: Principal::Owner {
            identity: "owner".into(),
        },
    }
}
fn data(root: &tempfile::TempDir) -> DataDir {
    DataDir::acquire(
        &root.path().join("db"),
        &Holder {
            kind: LockKind::Daemon,
            pid: std::process::id(),
            started_at: "synthetic".into(),
            binary_version: "test".into(),
            executable: "/synthetic/orch".into(),
        },
    )
    .unwrap()
}
fn filter() -> Filter {
    Filter::Tasks {
        status: None,
        goal: None,
    }
}
fn request(sort: Sort, limit: usize) -> PageRequest {
    PageRequest {
        sort,
        limit,
        with_count: true,
        ..PageRequest::default()
    }
}
fn task(id: &str, status: TaskStatus, priority: i64, tick: i64, goal: Option<&str>) -> Task {
    Task {
        id: id.into(),
        status,
        priority,
        goal_id: goal.map(str::to_owned),
        created_at: format!("2026-01-01T00:00:00.{tick:03}Z"),
        updated_at: format!("2026-01-01T00:00:00.{:03}Z", 9 - tick),
        ..make_task()
    }
}
fn insert(conn: &rusqlite::Connection, project: &str, t: &Task, position: u64) {
    let created = t
        .created_at
        .parse::<jiff::Timestamp>()
        .unwrap()
        .as_millisecond();
    let updated = t
        .updated_at
        .parse::<jiff::Timestamp>()
        .unwrap()
        .as_millisecond();
    conn.execute("INSERT INTO tasks(project_id,id,goal_id,title,description,status,priority,attempts,max_attempts,position,created_by_kind,created_at,updated_at,revision,landing_state) VALUES (?1,?2,?3,'','',?4,?5,0,3,?6,'owner',?7,?8,1,?9)", rusqlite::params![project,t.id,t.goal_id,t.status.as_str(),t.priority,i64::try_from(position).unwrap(),created,updated,if t.status == TaskStatus::Done { Some("skipped") } else {None}]).unwrap();
}
async fn save(file: &FileStorage, project: &str, t: Task) -> u64 {
    let id = t.id.clone();
    file.write(
        owner(project),
        Lane::Entities,
        Box::new(move |u| {
            u.create(Entity::Task(Box::new(t)))?;
            Ok(())
        }),
    )
    .await
    .unwrap();
    file.read(owner(project))
        .await
        .unwrap()
        .get(EntityKind::Task, &id)
        .await
        .unwrap()
        .unwrap()
        .position
}
const SORTS: [Sort; 6] = [
    Sort::Default,
    Sort::Id,
    Sort::Position,
    Sort::Priority,
    Sort::CreatedAt,
    Sort::UpdatedAt,
];

#[tokio::test]
async fn ordered_ids_counts_and_filters_match_real_file_reference() {
    let root = tempfile::tempdir().unwrap();
    let data = data(&root);
    let mut db = open_connection(&data, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    migrate(&data, &mut db).unwrap();
    let file = FileStorage::new(root.path().join("reference"));
    // Every status, repeated timestamps and priorities, reversed lexical IDs.
    for (i, status) in TaskStatus::ALL.into_iter().enumerate() {
        for j in 0..3 {
            let t = task(
                &format!("t{:02}", 30 - i * 3 - j),
                status,
                (j % 2) as i64,
                (j % 2) as i64,
                if j == 0 { None } else { Some("g' OR 1=1 --") },
            );
            let position = save(&file, "p", t.clone()).await;
            insert(&db, "p", &t, position);
            insert(&db, "other", &t, position);
        }
    }
    let pool = ReadPool::open(&data, Synchronous::Normal, 1).unwrap();
    let filters = [
        filter(),
        Filter::Tasks {
            status: Some(TaskStatus::Todo),
            goal: None,
        },
        Filter::Tasks {
            status: None,
            goal: Some("g' OR 1=1 --".into()),
        },
        Filter::Tasks {
            status: Some(TaskStatus::Done),
            goal: Some("g' OR 1=1 --".into()),
        },
        Filter::Tasks {
            status: None,
            goal: Some("missing".into()),
        },
    ];
    for f in filters {
        for sort in SORTS {
            for limit in [1, 2, 7, 100, 1000] {
                let req = request(sort, limit);
                let snapshot = file.read(owner("p")).await.unwrap();
                let mut file_req = req.clone();
                let mut after: Option<Continuation> = None;
                let mut seen = Vec::new();
                loop {
                    let got = pool
                        .read(deadline(), |tx| {
                            Ok(query(tx, "p", &f, &req, after.as_ref()))
                        })
                        .unwrap()
                        .unwrap();
                    let expected = snapshot.list(&f, &file_req).await.unwrap();
                    let ids: Vec<_> = expected
                        .items
                        .iter()
                        .map(|r| r.entity.id().to_owned())
                        .collect();
                    assert_eq!(got.ids, ids, "{f:?} {sort:?} {limit}");
                    assert_eq!(got.total, expected.total);
                    assert_eq!(got.next.is_some(), expected.next_cursor.is_some());
                    seen.extend(got.ids);
                    match got.next {
                        Some(next) => {
                            after = Some(next);
                            file_req.cursor = expected.next_cursor;
                        }
                        None => break,
                    }
                }
                assert_eq!(
                    seen.len(),
                    seen.iter().collect::<std::collections::BTreeSet<_>>().len()
                );
            }
        }
    }
    let tx = db.transaction().unwrap();
    let page = query(&tx, "absent", &filter(), &request(Sort::Default, 100), None).unwrap();
    assert!(page.ids.is_empty());
    assert_eq!(page.total, Some(0));
    let req = PageRequest::default();
    assert_eq!(req.limit, 100);
    assert_eq!(query(&tx, "p", &filter(), &req, None).unwrap().total, None);
}

#[test]
fn invalid_requests_and_misbound_continuations_are_refused() {
    let root = tempfile::tempdir().unwrap();
    let data = data(&root);
    let mut db = open_connection(&data, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    migrate(&data, &mut db).unwrap();
    for id in ["a", "b"] {
        insert(&db, "p", &task(id, TaskStatus::Todo, 0, 0, None), 1);
    }
    let tx = db.transaction().unwrap();
    for limit in [0, 1001, usize::MAX] {
        assert!(matches!(
            query(&tx, "p", &filter(), &request(Sort::Id, limit), None),
            Err(task_page::Error::Page(StoreError::InvalidPage(_)))
        ));
    }
    for sort in [Sort::Name, Sort::Attempt] {
        assert!(matches!(
            query(&tx, "p", &filter(), &request(sort, 1), None),
            Err(task_page::Error::Page(StoreError::InvalidPage(_)))
        ));
    }
    assert!(query(&tx, "p", &Filter::Agents, &request(Sort::Id, 1), None).is_err());
    let req = request(Sort::Id, 1);
    let first = query(&tx, "p", &filter(), &req, None).unwrap();
    let next = first.next.unwrap();
    for (project, f, r) in [
        ("other", filter(), req.clone()),
        (
            "p",
            Filter::Tasks {
                status: Some(TaskStatus::Todo),
                goal: None,
            },
            req.clone(),
        ),
        ("p", filter(), request(Sort::Position, 1)),
    ] {
        assert!(matches!(
            query(&tx, project, &f, &r, Some(&next)),
            Err(task_page::Error::Page(StoreError::InvalidCursor))
        ));
    }
    let unresolved = PageRequest {
        cursor: Some(orch_core::store::Cursor::new("opaque".into()).unwrap()),
        ..req
    };
    assert!(matches!(
        query(&tx, "p", &filter(), &unresolved, None),
        Err(task_page::Error::Page(StoreError::InvalidCursor))
    ));
    // Even when every non-ID component ties, every offered sort seeks by ID.
    for sort in SORTS {
        let req = request(sort, 1);
        let first = query(&tx, "p", &filter(), &req, None).unwrap();
        assert_eq!(first.ids, ["a"]);
        let last = query(&tx, "p", &filter(), &req, first.next.as_ref()).unwrap();
        assert_eq!(last.ids, ["b"]);
        assert!(last.next.is_none());
    }
}

#[test]
fn unsupported_sort_keeps_its_request_error_beside_a_supplied_continuation() {
    let root = tempfile::tempdir().unwrap();
    let data = data(&root);
    let mut db = open_connection(&data, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    migrate(&data, &mut db).unwrap();
    for id in ["a", "b"] {
        insert(&db, "p", &task(id, TaskStatus::Todo, 0, 0, None), 1);
    }
    let tx = db.transaction().unwrap();
    let next = query(&tx, "p", &filter(), &request(Sort::Id, 1), None)
        .unwrap()
        .next
        .unwrap();
    // The continuation is bound to Id, so validating it first would answer
    // InvalidCursor; the request's own error comes first.
    for sort in [Sort::Attempt, Sort::Name] {
        assert!(matches!(
            query(&tx, "p", &filter(), &request(sort, 1), Some(&next)),
            Err(task_page::Error::Page(StoreError::InvalidPage(_)))
        ));
        assert!(matches!(
            task_page::validate_request(&filter(), &request(sort, 1)),
            Err(StoreError::InvalidPage(_))
        ));
    }
    assert!(task_page::validate_request(&filter(), &request(Sort::Id, 1)).is_ok());
}

#[test]
fn decoded_continuations_validate_every_tuple_component_before_querying() {
    use rusqlite::types::Value;

    let root = tempfile::tempdir().unwrap();
    let data = data(&root);
    let mut db = open_connection(&data, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    migrate(&data, &mut db).unwrap();
    for id in ["a", "b"] {
        insert(&db, "p", &task(id, TaskStatus::Todo, 0, 0, None), 1);
    }
    let tx = db.transaction().unwrap();
    for sort in SORTS {
        let req = request(sort, 1);
        let first = query(&tx, "p", &filter(), &req, None).unwrap();
        let anchor = first.next.unwrap();
        let mut malformed = Vec::new();
        // Every truncated length, an extra component and a large decoded tuple.
        for len in 0..anchor.values.len() {
            malformed.push(anchor.values[..len].to_vec());
        }
        let mut extra = anchor.values.clone();
        extra.push(Value::Integer(0));
        malformed.push(extra);
        malformed.push(vec![Value::Integer(0); 1024]);
        // No SQLite coercions: NULL, REAL, BLOB and the other scalar type
        // are refused at every position, including the final text ID.
        for index in 0..anchor.values.len() {
            for wrong in [
                Value::Null,
                Value::Real(1.0),
                Value::Blob(vec![1]),
                if index + 1 == anchor.values.len() {
                    Value::Integer(1)
                } else {
                    Value::Text("1".into())
                },
            ] {
                let mut values = anchor.values.clone();
                values[index] = wrong;
                malformed.push(values);
            }
        }
        for values in malformed {
            assert!(
                matches!(
                    Continuation::from_decoded(
                        "p".into(),
                        filter(),
                        sort,
                        values.clone(),
                        "p",
                        &filter(),
                        sort,
                    ),
                    Err(StoreError::InvalidCursor)
                ),
                "sort={sort:?}, values={values:?}"
            );
            // Defense in depth: even an internally forged continuation must
            // fail with the typed error, never panic or reach SQLite coercion.
            let forged = Continuation {
                values,
                ..anchor.clone()
            };
            assert!(matches!(
                query(&tx, "p", &filter(), &req, Some(&forged)),
                Err(task_page::Error::Page(StoreError::InvalidCursor))
            ));
        }
        let decoded = Continuation::from_decoded(
            "p".into(),
            filter(),
            sort,
            anchor.values,
            "p",
            &filter(),
            sort,
        )
        .unwrap();
        // Limit and count are deliberately not part of the cursor binding.
        let mut changed_request = request(sort, 2);
        changed_request.with_count = true;
        let last = query(&tx, "p", &filter(), &changed_request, Some(&decoded)).unwrap();
        assert_eq!(last.ids, ["b"]);
        assert_eq!(last.total, Some(2));
        assert!(last.next.is_none());
    }
}

// Keep the final ID text and every prefix scalar integer so only the arity
// guard can reject these tuples. Truncating a row tuple drops its text ID and
// can accidentally test scalar validation instead of the length check.
fn wrong_length_tuples(width: usize) -> Vec<Vec<rusqlite::types::Value>> {
    use rusqlite::types::Value;

    (1..=width + 1)
        .filter(|len| *len != width)
        .map(|len| {
            std::iter::repeat_n(Value::Integer(0), len - 1)
                .chain([Value::Text("a".into())])
                .collect()
        })
        .collect()
}

fn assert_decoded_wrong_length_is_refused(sort: Sort, width: usize) {
    for values in wrong_length_tuples(width) {
        assert!(
            matches!(
                Continuation::from_decoded(
                    "p".into(),
                    filter(),
                    sort,
                    values.clone(),
                    "p",
                    &filter(),
                    sort,
                ),
                Err(StoreError::InvalidCursor)
            ),
            "sort={sort:?}, values={values:?}"
        );
    }
}

fn assert_query_wrong_length_is_refused(sort: Sort, width: usize) {
    let root = tempfile::tempdir().unwrap();
    let data = data(&root);
    let mut db = open_connection(&data, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    migrate(&data, &mut db).unwrap();
    insert(&db, "p", &task("b", TaskStatus::Todo, 0, 0, None), 1);
    let pool = ReadPool::open(&data, Synchronous::Normal, 1).unwrap();
    let req = request(sort, 1);
    for values in wrong_length_tuples(width) {
        let forged = Continuation {
            project: "p".into(),
            filter: filter(),
            sort,
            values,
        };
        let rejected = pool
            .read(deadline(), |tx| {
                Ok(query(tx, "p", &filter(), &req, Some(&forged)))
            })
            .unwrap();
        assert!(
            matches!(
                rejected,
                Err(task_page::Error::Page(StoreError::InvalidCursor))
            ),
            "sort={sort:?}, values={:?}, result={rejected:?}",
            forged.values
        );
        // Rejection must return the only read slot without poisoning it.
        let recovered = pool
            .read(deadline(), |tx| Ok(query(tx, "p", &filter(), &req, None)))
            .unwrap()
            .unwrap();
        assert_eq!(recovered.ids, ["b"]);
        assert_eq!(recovered.total, Some(1));
    }
}

macro_rules! wrong_length_regressions {
    ($decoded:ident, $query:ident, $sort:expr, $width:expr) => {
        #[test]
        fn $decoded() {
            assert_decoded_wrong_length_is_refused($sort, $width);
        }
        #[test]
        fn $query() {
            assert_query_wrong_length_is_refused($sort, $width);
        }
    };
}

// Widths are literal contract expectations, independent of sort_keys().
wrong_length_regressions!(
    decoded_wrong_length_default,
    query_wrong_length_default,
    Sort::Default,
    4
);
wrong_length_regressions!(decoded_wrong_length_id, query_wrong_length_id, Sort::Id, 1);
wrong_length_regressions!(
    decoded_wrong_length_position,
    query_wrong_length_position,
    Sort::Position,
    2
);
wrong_length_regressions!(
    decoded_wrong_length_priority,
    query_wrong_length_priority,
    Sort::Priority,
    3
);
wrong_length_regressions!(
    decoded_wrong_length_created_at,
    query_wrong_length_created_at,
    Sort::CreatedAt,
    3
);
wrong_length_regressions!(
    decoded_wrong_length_updated_at,
    query_wrong_length_updated_at,
    Sort::UpdatedAt,
    3
);

#[test]
fn decoded_continuations_refuse_mismatched_bindings_and_unsupported_sorts() {
    use rusqlite::types::Value;

    for sort in SORTS {
        let values = vec![Value::Integer(0); super::sort_keys(sort).unwrap().len() - 1]
            .into_iter()
            .chain([Value::Text("a".into())])
            .collect::<Vec<_>>();
        let other_sort = if sort == Sort::Id {
            Sort::Position
        } else {
            Sort::Id
        };
        for (project, decoded_filter, decoded_sort) in [
            ("other", filter(), sort),
            (
                "p",
                Filter::Tasks {
                    status: Some(TaskStatus::Todo),
                    goal: None,
                },
                sort,
            ),
            (
                "p",
                Filter::Tasks {
                    status: None,
                    goal: Some("other".into()),
                },
                sort,
            ),
            ("p", Filter::Agents, sort),
            ("p", filter(), other_sort),
            ("p", filter(), Sort::Name),
            ("p", filter(), Sort::Attempt),
        ] {
            assert!(matches!(
                Continuation::from_decoded(
                    project.into(),
                    decoded_filter,
                    decoded_sort,
                    values.clone(),
                    "p",
                    &filter(),
                    sort,
                ),
                Err(StoreError::InvalidCursor)
            ));
        }
    }
    // Attempt is a Runs-only sort: even a self-consistent decoded Attempt
    // continuation is refused as a cursor, as Name is.
    for (f, sort) in [
        (Filter::Agents, Sort::Id),
        (filter(), Sort::Name),
        (filter(), Sort::Attempt),
    ] {
        assert!(matches!(
            Continuation::from_decoded(
                "p".into(),
                f.clone(),
                sort,
                vec![Value::Text("a".into())],
                "p",
                &f,
                sort
            ),
            Err(StoreError::InvalidCursor)
        ));
    }
}

#[test]
fn malformed_decoded_continuation_keeps_read_pool_available() {
    use rusqlite::types::Value;

    let root = tempfile::tempdir().unwrap();
    let data = data(&root);
    let mut db = open_connection(&data, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    migrate(&data, &mut db).unwrap();
    insert(&db, "p", &task("a", TaskStatus::Todo, 0, 0, None), 1);
    let pool = ReadPool::open(&data, Synchronous::Normal, 1).unwrap();
    let forged = Continuation {
        project: "p".into(),
        filter: filter(),
        sort: Sort::Id,
        values: vec![Value::Null],
    };
    let refused = pool
        .read(deadline(), |tx| {
            Ok(query(
                tx,
                "p",
                &filter(),
                &request(Sort::Id, 1),
                Some(&forged),
            ))
        })
        .unwrap();
    assert!(matches!(
        refused,
        Err(task_page::Error::Page(StoreError::InvalidCursor))
    ));
    let next = pool
        .read(deadline(), |tx| {
            Ok(query(tx, "p", &filter(), &request(Sort::Id, 1), None))
        })
        .unwrap()
        .unwrap();
    assert_eq!(next.ids, ["a"]);
}

#[tokio::test]
async fn keyset_walk_survives_insert_and_deleted_anchor_for_every_sort() {
    for sort in SORTS {
        let root = tempfile::tempdir().unwrap();
        let data = data(&root);
        let mut db = open_connection(&data, ConnectionRole::Writer, Synchronous::Normal).unwrap();
        migrate(&data, &mut db).unwrap();
        let file = FileStorage::new(root.path().join("reference"));
        for id in ["a", "c", "e", "g"] {
            let t = task(id, TaskStatus::Todo, 0, 0, None);
            let position = save(&file, "p", t.clone()).await;
            insert(&db, "p", &t, position);
        }
        let req = request(sort, 2);
        let tx = db.transaction().unwrap();
        let first = query(&tx, "p", &filter(), &req, None).unwrap();
        tx.commit().unwrap();
        let expected = file
            .read(owner("p"))
            .await
            .unwrap()
            .list(&filter(), &req)
            .await
            .unwrap();
        assert_eq!(
            first.ids,
            expected
                .items
                .iter()
                .map(|r| r.entity.id().to_owned())
                .collect::<Vec<_>>()
        );
        let anchor = first.ids.last().unwrap().clone();
        db.execute("DELETE FROM tasks WHERE project_id='p' AND id=?", [&anchor])
            .unwrap();
        file.write(
            owner("p"),
            Lane::Entities,
            Box::new(move |u| u.delete(EntityKind::Task, &anchor, Revision::ONE)),
        )
        .await
        .unwrap();
        for id in ["0", "z"] {
            let t = task(id, TaskStatus::Todo, 0, 0, None);
            let position = save(&file, "p", t.clone()).await;
            insert(&db, "p", &t, position);
        }
        let mut after = first.next;
        let mut file_req = PageRequest {
            cursor: expected.next_cursor,
            ..req.clone()
        };
        let mut seen = first.ids;
        loop {
            let tx = db.transaction().unwrap();
            let got = query(&tx, "p", &filter(), &req, after.as_ref()).unwrap();
            tx.commit().unwrap();
            let expected = file
                .read(owner("p"))
                .await
                .unwrap()
                .list(&filter(), &file_req)
                .await
                .unwrap();
            assert_eq!(
                got.ids,
                expected
                    .items
                    .iter()
                    .map(|r| r.entity.id().to_owned())
                    .collect::<Vec<_>>()
            );
            assert_eq!(got.total, expected.total);
            seen.extend(got.ids);
            after = got.next;
            file_req.cursor = expected.next_cursor;
            if after.is_none() {
                break;
            }
        }
        for id in ["a", "c", "e", "g"] {
            assert_eq!(seen.iter().filter(|s| s.as_str() == id).count(), 1);
        }
    }
}

#[test]
fn snapshot_stays_consistent_across_external_commit() {
    let root = tempfile::tempdir().unwrap();
    let data = data(&root);
    let mut writer = open_connection(&data, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    migrate(&data, &mut writer).unwrap();
    insert(&writer, "p", &task("a", TaskStatus::Todo, 0, 0, None), 1);
    let pool = ReadPool::open(&data, Synchronous::Normal, 1).unwrap();
    let req = request(Sort::Id, 100);
    pool.read(deadline(), |tx| {
        assert_eq!(
            query(tx, "p", &filter(), &req, None).unwrap().total,
            Some(1)
        );
        insert(&writer, "p", &task("b", TaskStatus::Todo, 0, 0, None), 2);
        let again = query(tx, "p", &filter(), &req, None).unwrap();
        assert_eq!(again.total, Some(1));
        assert_eq!(again.ids, ["a"]);
        Ok(())
    })
    .unwrap();
    let next = pool
        .read(deadline(), |tx| Ok(query(tx, "p", &filter(), &req, None)))
        .unwrap()
        .unwrap();
    assert_eq!(next.total, Some(2));
}

#[test]
fn default_and_maximum_pages_are_bounded_and_count_is_opt_in() {
    let root = tempfile::tempdir().unwrap();
    let data = data(&root);
    let mut db = open_connection(&data, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    migrate(&data, &mut db).unwrap();
    for i in 0..1001 {
        insert(
            &db,
            "p",
            &task(&format!("t{i:04}"), TaskStatus::Todo, i64::MIN, 0, None),
            i + 1,
        );
    }
    let tx = db.transaction().unwrap();
    let first = query(&tx, "p", &filter(), &PageRequest::default(), None).unwrap();
    assert_eq!(first.ids.len(), 100);
    assert!(first.next.is_some());
    assert_eq!(first.total, None);
    let req = request(Sort::Priority, 1000);
    let first = query(&tx, "p", &filter(), &req, None).unwrap();
    assert_eq!(first.ids.len(), 1000);
    assert_eq!(first.total, Some(1001));
    let last = query(&tx, "p", &filter(), &req, first.next.as_ref()).unwrap();
    assert_eq!(last.ids, ["t1000"]);
    assert_eq!(last.total, Some(1001));
    assert!(last.next.is_none());
}

#[tokio::test]
async fn mixed_timestamp_precision_is_a_documented_incompatible_representation() {
    let root = tempfile::tempdir().unwrap();
    let data = data(&root);
    let mut db = open_connection(&data, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    migrate(&data, &mut db).unwrap();
    let file = FileStorage::new(root.path().join("reference"));
    let mut a = task("a", TaskStatus::Todo, 0, 0, None);
    a.created_at = "2026-01-01T00:00:00Z".into();
    let mut b = task("b", TaskStatus::Todo, 0, 0, None);
    b.created_at = "2026-01-01T00:00:00.001Z".into();
    for t in [a, b] {
        let position = save(&file, "p", t.clone()).await;
        insert(&db, "p", &t, position);
    }
    let req = request(Sort::CreatedAt, 100);
    let tx = db.transaction().unwrap();
    let got = query(&tx, "p", &filter(), &req, None).unwrap();
    let expected = file
        .read(owner("p"))
        .await
        .unwrap()
        .list(&filter(), &req)
        .await
        .unwrap();
    assert_eq!(got.ids, ["a", "b"]);
    assert_eq!(
        expected
            .items
            .iter()
            .map(|r| r.entity.id())
            .collect::<Vec<_>>(),
        ["b", "a"]
    );
    // Offset and sub-millisecond strings also lose raw lexical information.
    let left = "2026-01-01T00:30:00+01:00";
    let right = "2025-12-31T23:45:00Z";
    assert!(left > right);
    assert!(left.parse::<jiff::Timestamp>().unwrap() < right.parse::<jiff::Timestamp>().unwrap());
    let left = "2026-01-01T00:00:00.0001Z";
    let right = "2026-01-01T00:00:00.0009Z";
    assert_ne!(left, right);
    assert_eq!(
        left.parse::<jiff::Timestamp>().unwrap().as_millisecond(),
        right.parse::<jiff::Timestamp>().unwrap().as_millisecond()
    );
}

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(5)
}

#[test]
fn read_pool_preserves_query_errors_and_returns_its_slot() {
    let root = tempfile::tempdir().unwrap();
    let data = data(&root);
    let mut writer = open_connection(&data, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    migrate(&data, &mut writer).unwrap();
    let pool = ReadPool::open(&data, Synchronous::Normal, 1).unwrap();
    let invalid = pool
        .read(deadline(), |tx| {
            Ok(query(tx, "p", &filter(), &request(Sort::Id, 0), None))
        })
        .unwrap();
    assert!(matches!(
        invalid,
        Err(task_page::Error::Page(StoreError::InvalidPage(_)))
    ));

    // A malformed stored schema must reach the callback caller as a SQL error,
    // not an empty page. Forward it through the pool's existing error channel.
    writer
        .execute("ALTER TABLE tasks RENAME TO unavailable_tasks", [])
        .unwrap();
    let failed: crate::sqlite::Result<()> = pool.read(deadline(), |tx| {
        match query(tx, "p", &filter(), &request(Sort::Id, 1), None) {
            Err(task_page::Error::Sql(error)) => Err(SqliteError::Sql(error)),
            other => panic!("expected SQL failure, got {other:?}"),
        }
    });
    assert!(matches!(failed, Err(SqliteError::Sql(_))));
    writer
        .execute("ALTER TABLE unavailable_tasks RENAME TO tasks", [])
        .unwrap();
    insert(&writer, "p", &task("a", TaskStatus::Todo, 0, 0, None), 1);
    let recovered = pool
        .read(deadline(), |tx| {
            Ok(query(tx, "p", &filter(), &request(Sort::Id, 1), None))
        })
        .unwrap()
        .unwrap();
    assert_eq!(recovered.ids, ["a"]);
    assert_eq!(recovered.total, Some(1));
}
