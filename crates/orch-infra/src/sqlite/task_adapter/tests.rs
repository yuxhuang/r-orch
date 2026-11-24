use super::cursor::TTL_MS;
use super::*;
use crate::sqlite::FailureState;
use crate::sqlite::task_sql::writes::TaskWrite;
use crate::sqlite::{
    ConnectionRole, Lane, Synchronous, Writer, WriterHandle, migrations, open_connection,
};
use crate::storage::v2::FileStorage;
use orch_core::store::{Caller, Cursor, Entity, Principal, Revision, Sort, Storage, StoreError};
use orch_core::test_support::{FixedClock, make_task};
use orch_domain::{Task, TaskStatus};
use std::sync::mpsc;
use std::time::Duration;

const SORTS: [Sort; 6] = [
    Sort::Default,
    Sort::Id,
    Sort::Position,
    Sort::Priority,
    Sort::CreatedAt,
    Sort::UpdatedAt,
];

fn owner() -> Principal {
    Principal::Owner {
        identity: "synthetic".into(),
    }
}

fn filter() -> Filter {
    Filter::Tasks {
        status: None,
        goal: None,
    }
}

fn request(sort: Sort, limit: usize) -> PageRequest {
    PageRequest {
        limit,
        cursor: None,
        sort,
        with_count: true,
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

/// Every status, repeated priorities and times, IDs that disagree with every
/// other order, two goals.
fn tasks() -> Vec<Task> {
    use TaskStatus::*;
    vec![
        task("t3", Todo, 2, 3, Some("g1")),
        task("t1", Done, 2, 1, None),
        task("t9", InProgress, 1, 5, Some("g1")),
        task("t2", Todo, 2, 2, Some("g2")),
        task("t7", Failed, 5, 5, None),
        task("t5", Review, 1, 0, Some("g1")),
        task("t4", Retrying, 3, 4, None),
        task("t8", Cancelled, 2, 6, None),
        task("t6", Todo, 0, 7, Some("g1")),
    ]
}

fn filters() -> Vec<Filter> {
    vec![
        filter(),
        Filter::Tasks {
            status: Some(TaskStatus::Todo),
            goal: None,
        },
        Filter::Tasks {
            status: None,
            goal: Some("g1".into()),
        },
        Filter::Tasks {
            status: Some(TaskStatus::Todo),
            goal: Some("g1".into()),
        },
    ]
}

fn invalid_cursor<T: std::fmt::Debug>(result: Result<T>) {
    assert!(
        matches!(result, Err(SqliteError::Store(StoreError::InvalidCursor))),
        "{result:?}"
    );
}

fn invalid_page<T: std::fmt::Debug>(result: Result<T>) {
    assert!(
        matches!(result, Err(SqliteError::Store(StoreError::InvalidPage(_)))),
        "{result:?}"
    );
}

fn submit<T: Send + 'static>(
    handle: &WriterHandle,
    work: impl FnOnce(&rusqlite::Transaction<'_>) -> Result<T> + Send + 'static,
) -> Result<T> {
    let receipt = handle.submit(Lane::Entities, None, work)?;
    let (send, recv) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = send.send(receipt.wait());
    });
    recv.recv_timeout(Duration::from_secs(15))
        .expect("writer receipt deadline")
}

struct Fixture<'a> {
    handle: &'a WriterHandle,
    state: &'a FailureState,
    disk: &'a Arc<DiskBlobStore>,
    pool: &'a ReadPool<'a>,
    clock: &'a Arc<FixedClock>,
    adapter: &'a TaskAdapter<'a, 'a>,
}

impl Fixture<'_> {
    fn write<T: Send + 'static>(
        &self,
        work: impl FnOnce(&mut TaskWrite<'_, '_>) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let (disk, state) = (self.disk.clone(), self.state.clone());
        submit(self.handle, move |tx| {
            let caller = owner();
            let sql = TaskSql {
                tx,
                project: "p",
                state,
                source: FailureSource::Writer,
                disk: &disk,
            };
            let mut write = TaskWrite::new(sql, &caller);
            let out = work(&mut write)?;
            write.ensure_committable()?;
            Ok(out)
        })
    }

    fn seed(&self, tasks: Vec<Task>) {
        self.write(move |write| {
            for task in tasks {
                write.create(task)?;
            }
            Ok(())
        })
        .unwrap();
    }

    fn list(&self, filter: &Filter, request: &PageRequest) -> Result<Page<Record>> {
        self.adapter.list(
            "p",
            filter,
            request,
            Instant::now() + Duration::from_secs(15),
        )
    }

    fn walk(&self, filter: &Filter, mut request: PageRequest) -> Vec<Page<Record>> {
        let mut pages = Vec::new();
        loop {
            let page = self.list(filter, &request).unwrap();
            request.cursor = page.next_cursor.clone();
            pages.push(page);
            if request.cursor.is_none() {
                return pages;
            }
        }
    }

    fn ids(page: &Page<Record>) -> Vec<String> {
        page.items
            .iter()
            .map(|r| r.entity.id().to_owned())
            .collect()
    }
}

fn scenario(pool_size: usize, body: impl FnOnce(&Fixture<'_>)) {
    let (_temp, data) = crate::sqlite::test_data();
    let disk = Arc::new(DiskBlobStore::open(data.root()).unwrap());
    let mut setup = open_connection(&data, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    migrations::migrate(&data, &mut setup).unwrap();
    drop(setup);
    let state = FailureState::new();
    let (writer, handle) =
        Writer::open_with_state(&data, Synchronous::Normal, 8, state.clone()).unwrap();
    let pool =
        ReadPool::open_with_state(&data, Synchronous::Normal, pool_size, state.clone()).unwrap();
    let clock = Arc::new(FixedClock::new());
    let adapter = TaskAdapter::new(&pool, &disk, "generation-1", clock.clone());
    std::thread::scope(|scope| {
        let worker = scope.spawn(|| writer.run());
        struct Close<'a>(&'a WriterHandle);
        impl Drop for Close<'_> {
            fn drop(&mut self) {
                self.0.close();
            }
        }
        let _close = Close(&handle);
        body(&Fixture {
            handle: &handle,
            state: &state,
            disk: &disk,
            pool: &pool,
            clock: &clock,
            adapter: &adapter,
        });
        handle.close();
        worker.join().unwrap();
    });
}

fn caller() -> Caller {
    Caller {
        project: "p".into(),
        principal: owner(),
    }
}

/// The file reference's pages for every query, in the order `tests` walks them.
async fn reference(tasks: &[Task]) -> Vec<Vec<Page<Record>>> {
    let root = tempfile::tempdir().unwrap();
    let file = FileStorage::new(root.path().join("reference"));
    for task in tasks {
        let task = task.clone();
        file.write(
            caller(),
            orch_core::store::Lane::Entities,
            Box::new(move |u| {
                u.create(Entity::Task(Box::new(task)))?;
                Ok(())
            }),
        )
        .await
        .unwrap();
    }
    let snapshot = file.read(caller()).await.unwrap();
    let mut all = Vec::new();
    for filter in filters() {
        for sort in SORTS {
            for limit in [1, 2, 4, 1000] {
                let mut req = request(sort, limit);
                let mut pages = Vec::new();
                loop {
                    let page = snapshot.list(&filter, &req).await.unwrap();
                    req.cursor = page.next_cursor.clone();
                    pages.push(page);
                    if req.cursor.is_none() {
                        break;
                    }
                }
                all.push(pages);
            }
        }
    }
    all
}

#[tokio::test]
async fn all_six_sorts_page_with_hydrated_records_like_the_file_reference() {
    let tasks = tasks();
    let expected = reference(&tasks).await;
    scenario(1, |fx| {
        fx.seed(tasks.clone());
        let mut expected = expected.iter();
        for filter in filters() {
            for sort in SORTS {
                for limit in [1, 2, 4, 1000] {
                    let want = expected.next().unwrap();
                    let got = fx.walk(&filter, request(sort, limit));
                    assert_eq!(got.len(), want.len(), "{filter:?} {sort:?} {limit}");
                    for (got, want) in got.iter().zip(want) {
                        // Whole records: ID order, revision, position and every
                        // field of the decoded task.
                        assert_eq!(got.items, want.items, "{filter:?} {sort:?} {limit}");
                        assert_eq!(got.total, want.total);
                        assert_eq!(got.next_cursor.is_some(), want.next_cursor.is_some());
                        if let Some(token) = &got.next_cursor {
                            assert!(token.as_str().len() <= 512);
                        }
                    }
                }
            }
        }
    });
}

#[test]
fn a_walk_returns_every_unchanged_row_once_across_inserts_and_deletes() {
    scenario(1, |fx| {
        fx.seed(tasks());
        let first = fx.list(&filter(), &request(Sort::Id, 3)).unwrap();
        assert_eq!(Fixture::ids(&first), ["t1", "t2", "t3"]);
        fx.write(|write| {
            // Before the cursor, after it, and a deletion of a row not yet seen.
            write.create(task("t0", TaskStatus::Todo, 1, 1, None))?;
            write.create(task("tz", TaskStatus::Todo, 1, 1, None))?;
            write.delete("t7", Revision::ONE)
        })
        .unwrap();
        let mut seen = Fixture::ids(&first);
        let mut req = request(Sort::Id, 3);
        req.cursor = first.next_cursor;
        while req.cursor.is_some() {
            let page = fx.list(&filter(), &req).unwrap();
            seen.extend(Fixture::ids(&page));
            req.cursor = page.next_cursor;
        }
        assert_eq!(seen, ["t1", "t2", "t3", "t4", "t5", "t6", "t8", "t9", "tz"]);
    });
}

#[test]
fn request_errors_are_reported_before_any_cursor_is_decoded() {
    scenario(1, |fx| {
        fx.seed(tasks());
        let garbage = Some(Cursor::new("not a token".into()).unwrap());
        let with = |sort, limit| PageRequest {
            limit,
            cursor: garbage.clone(),
            sort,
            with_count: false,
        };
        invalid_page(fx.list(&filter(), &with(Sort::Default, 0)));
        invalid_page(fx.list(&filter(), &with(Sort::Default, 1001)));
        invalid_page(fx.list(&filter(), &with(Sort::Attempt, 10)));
        invalid_page(fx.list(&filter(), &with(Sort::Name, 10)));
        invalid_page(fx.list(&Filter::Agents, &with(Sort::Default, 10)));
        // A token over the public bound is the request's own error, and the
        // limit is still checked first.
        let long: Cursor = serde_json::from_str(&format!("\"{}\"", "a".repeat(513))).unwrap();
        let oversized = |limit| PageRequest {
            limit,
            cursor: Some(long.clone()),
            ..request(Sort::Default, 10)
        };
        invalid_page(fx.list(&filter(), &oversized(0)));
        invalid_cursor(fx.list(&filter(), &oversized(10)));
        // The same garbage is only a cursor error on an otherwise valid request.
        invalid_cursor(fx.list(&filter(), &with(Sort::Default, 10)));
        // None of it disturbed the single read slot.
        assert_eq!(
            fx.list(&filter(), &request(Sort::Id, 2))
                .unwrap()
                .items
                .len(),
            2
        );
    });
}

fn first_token(fx: &Fixture<'_>, filter: &Filter, sort: Sort) -> Cursor {
    fx.list(filter, &request(sort, 2))
        .unwrap()
        .next_cursor
        .expect("more than one page")
}

fn resumed(token: &Cursor, sort: Sort) -> PageRequest {
    PageRequest {
        cursor: Some(token.clone()),
        ..request(sort, 2)
    }
}

#[test]
fn tokens_are_bound_to_filter_sort_project_generation_and_lifetime() {
    scenario(1, |fx| {
        fx.seed(tasks());
        let bound = Filter::Tasks {
            status: Some(TaskStatus::Todo),
            goal: None,
        };
        let token = first_token(fx, &bound, Sort::Priority);
        let ok = |request: &PageRequest| fx.list(&bound, request).unwrap();
        assert!(!ok(&resumed(&token, Sort::Priority)).items.is_empty());

        for sort in SORTS.into_iter().filter(|s| *s != Sort::Priority) {
            invalid_cursor(fx.list(&bound, &resumed(&token, sort)));
        }
        for other in [
            filter(),
            Filter::Tasks {
                status: Some(TaskStatus::Done),
                goal: None,
            },
            Filter::Tasks {
                status: Some(TaskStatus::Todo),
                goal: Some("g1".into()),
            },
        ] {
            invalid_cursor(fx.list(&other, &resumed(&token, Sort::Priority)));
        }
        invalid_cursor(fx.adapter.list(
            "q",
            &bound,
            &resumed(&token, Sort::Priority),
            Instant::now() + Duration::from_secs(15),
        ));
        // A restore starts a new generation over the same data.
        let restored = TaskAdapter::new(fx.pool, fx.disk, "generation-2", fx.clock.clone());
        invalid_cursor(restored.list(
            "p",
            &bound,
            &resumed(&token, Sort::Priority),
            Instant::now() + Duration::from_secs(15),
        ));

        // The claim must be inside the lifetime that the generation could issue:
        // valid through the last millisecond, refused after it.
        fx.clock.advance_ms(TTL_MS);
        assert!(!ok(&resumed(&token, Sort::Priority)).items.is_empty());
        fx.clock.advance_ms(1);
        invalid_cursor(fx.list(&bound, &resumed(&token, Sort::Priority)));
        fx.clock.advance_ms(-TTL_MS - 1);

        // One that outlives any token the generation could have issued.
        let cursors = TaskCursors::new("generation-1");
        let next = task_page_continuation(fx, &bound, Sort::Priority);
        let future = cursors
            .issue(
                "p",
                &bound,
                Sort::Priority,
                &next,
                fx.clock.now_ms() + 2 * TTL_MS,
            )
            .unwrap();
        invalid_cursor(fx.list(&bound, &resumed(&future, Sort::Priority)));

        // Every refusal above left the pool usable and the state unlatched.
        assert!(!ok(&resumed(&token, Sort::Priority)).items.is_empty());
        fx.state.ensure_available().unwrap();
    });
}

fn task_page_continuation(
    fx: &Fixture<'_>,
    filter: &Filter,
    sort: Sort,
) -> task_page::Continuation {
    fx.pool
        .read(Instant::now() + Duration::from_secs(15), |tx| {
            Ok(task_page::query(tx, "p", filter, &request(sort, 2), None)
                .unwrap()
                .next
                .unwrap())
        })
        .unwrap()
}

fn hex(raw: &[u8]) -> String {
    raw.iter().map(|b| format!("{b:02x}")).collect()
}

fn raw(token: &Cursor) -> Vec<u8> {
    (0..token.as_str().len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&token.as_str()[i..i + 2], 16).unwrap())
        .collect()
}

fn cursor(text: String) -> Cursor {
    Cursor::new(text).unwrap()
}

#[test]
fn malformed_tampered_and_misshapen_tokens_are_refused_without_wedging_the_pool() {
    scenario(1, |fx| {
        fx.seed(tasks());
        let sort = Sort::Priority;
        let token = first_token(fx, &filter(), sort);
        let good = raw(&token);
        let try_raw = |bytes: &[u8]| fx.list(&filter(), &resumed(&cursor(hex(bytes)), sort));
        let try_text = |text: &str| fx.list(&filter(), &resumed(&cursor(text.into()), sort));
        try_raw(&good).unwrap();
        // Hex spelling: odd length, a non-hex digit, upper case (one position
        // has one token) and a different encoding of the same bytes.
        invalid_cursor(try_text(&token.as_str()[1..]));
        invalid_cursor(try_text(&format!("{}zz", token.as_str())));
        invalid_cursor(try_text(&token.as_str().to_uppercase()));
        invalid_cursor(try_text("ab"));
        // Header: version, sort tag, a high expiry byte, any digest byte.
        for index in (0..2).chain(2..5).chain(10..26) {
            let mut bad = good.clone();
            bad[index] ^= 0x80;
            invalid_cursor(try_raw(&bad));
        }
        // A sort tag that names no sort, and one that names another sort.
        for tag in [6, 1] {
            let mut bad = good.clone();
            bad[1] = tag;
            invalid_cursor(try_raw(&bad));
        }
        // Shorter than the header, and without the integer keys of its sort.
        invalid_cursor(try_raw(&good[..25]));
        invalid_cursor(try_raw(&good[..26 + 7]));
        // An ID that is not UTF-8.
        let mut bad = good[..26 + 16].to_vec();
        bad.push(0xff);
        invalid_cursor(try_raw(&bad));
        // The token holds a position, not a snapshot, and no secret: another
        // position of the same query is a position the caller can reach anyway.
        let mut forged = good.clone();
        *forged.last_mut().unwrap() = b'0';
        try_raw(&forged).unwrap();
        // Still usable, never latched.
        try_raw(&good).unwrap();
        fx.state.ensure_available().unwrap();
    });
}

#[test]
fn rejected_cursor_leaves_the_writer_and_its_state_usable() {
    scenario(1, |fx| {
        fx.seed(tasks());
        let token = first_token(fx, &filter(), Sort::Position);
        let (disk, state) = (fx.disk.clone(), fx.state.clone());
        let refused = submit(fx.handle, move |tx| {
            let sql = TaskSql {
                tx,
                project: "p",
                state,
                source: FailureSource::Writer,
                disk: &disk,
            };
            tx.execute("UPDATE tasks SET priority = 99 WHERE id = 't1'", [])?;
            // This token was issued for another sort.
            let request = PageRequest {
                cursor: Some(token),
                ..request(Sort::Id, 2)
            };
            page(
                &sql,
                &TaskCursors::new("generation-1"),
                0,
                &filter(),
                &request,
            )
        });
        invalid_cursor(refused);
        fx.state.ensure_available().unwrap();
        // The refused callback rolled back whole.
        let all = fx.list(&filter(), &request(Sort::Id, 1000)).unwrap();
        let Entity::Task(t1) = &all.items[0].entity else {
            unreachable!()
        };
        assert_eq!((t1.id.as_str(), t1.priority), ("t1", 2));
        // The next callback commits, and its page sees its own uncommitted write
        // and resumes its own token inside the same transaction.
        let (disk, state) = (fx.disk.clone(), fx.state.clone());
        let ids = submit(fx.handle, move |tx| {
            let caller = owner();
            let sql = |state: &FailureState| TaskSql {
                tx,
                project: "p",
                state: state.clone(),
                source: FailureSource::Writer,
                disk: &disk,
            };
            let mut write = TaskWrite::new(sql(&state), &caller);
            write.create(task("tn", TaskStatus::Todo, 1, 1, None))?;
            let cursors = TaskCursors::new("generation-1");
            let sql = sql(&state);
            let first = page(&sql, &cursors, 0, &filter(), &request(Sort::Position, 9))?;
            let rest = PageRequest {
                cursor: first.next_cursor,
                ..request(Sort::Position, 9)
            };
            let second = page(&sql, &cursors, 0, &filter(), &rest)?;
            write.ensure_committable()?;
            Ok((first.items.len(), Fixture::ids(&second)))
        })
        .unwrap();
        assert_eq!(ids, (9, vec!["tn".to_owned()]));
        fx.state.ensure_available().unwrap();
    });
}

#[test]
fn a_page_hydrates_the_selected_ids_from_one_snapshot_across_a_concurrent_commit() {
    scenario(2, |fx| {
        fx.seed(tasks());
        let (disk, state) = (fx.disk.clone(), fx.state.clone());
        let cursors = TaskCursors::new("generation-1");
        let now = fx.clock.now_ms();
        let page = fx
            .pool
            .read(Instant::now() + Duration::from_secs(15), |tx| {
                let sql = TaskSql {
                    tx,
                    project: "p",
                    state: state.clone(),
                    source: FailureSource::Reader,
                    disk: &disk,
                };
                // After the IDs and the count are selected, another connection
                // updates one selected task, deletes another and adds a row.
                page_between(
                    &sql,
                    &cursors,
                    now,
                    &filter(),
                    &request(Sort::Position, 4),
                    || {
                        fx.write(|write| {
                            let mut renamed = task("t3", TaskStatus::Todo, 2, 3, Some("g1"));
                            renamed.title = "renamed after selection".into();
                            write.update(renamed, Revision::ONE)?;
                            write.delete("t1", Revision::ONE)?;
                            write.create(task("t00", TaskStatus::Todo, 1, 1, None))?;
                            Ok(())
                        })
                        .unwrap();
                    },
                )
            })
            .unwrap();
        // The old snapshot, in the order selected: both rows as they were, the
        // original count, and no row that was added later.
        assert_eq!(Fixture::ids(&page), ["t3", "t1", "t9", "t2"]);
        assert_eq!(page.total, Some(9));
        assert_eq!(page.items[0].revision, Revision::ONE);
        let Entity::Task(first) = &page.items[0].entity else {
            unreachable!()
        };
        assert_eq!(first.title, "Test task");
        // The cursor continues the snapshot's last row in the next one.
        let next = fx
            .list(
                &filter(),
                &PageRequest {
                    cursor: page.next_cursor,
                    ..request(Sort::Position, 100)
                },
            )
            .unwrap();
        assert_eq!(Fixture::ids(&next), ["t7", "t5", "t4", "t8", "t6", "t00"]);
        assert_eq!(next.total, Some(9));
        let fresh = fx.list(&filter(), &request(Sort::Position, 1)).unwrap();
        assert_eq!(fresh.items[0].revision, Revision::ONE.next().unwrap());
    });
}

#[test]
fn one_damaged_record_fails_the_whole_page_and_the_shared_state() {
    scenario(1, |fx| {
        fx.seed(tasks());
        submit(fx.handle, |tx| {
            tx.execute("DELETE FROM task_details WHERE task_id = 't9'", [])?;
            Ok(())
        })
        .unwrap();
        // A page that does not select the damaged row is unaffected.
        let before = fx.list(&filter(), &request(Sort::Position, 2)).unwrap();
        assert_eq!(Fixture::ids(&before), ["t3", "t1"]);
        fx.state.ensure_available().unwrap();
        // One that does returns the error and no records, not a short page.
        let error = fx.list(&filter(), &request(Sort::Position, 5)).unwrap_err();
        assert!(
            matches!(error, SqliteError::TaskCorrupt { .. }),
            "{error:?}"
        );
        assert!(fx.state.ensure_available().is_err());
    });
}

#[test]
fn a_last_id_that_does_not_fit_in_a_512_byte_token_is_refused_not_truncated() {
    scenario(1, |fx| {
        // Sort::Id carries only the ID: 26 header bytes and two hex digits per
        // byte leave 230 bytes of ID in 512 bytes of token.
        let id = |n: usize, c: &str| c.repeat(n);
        fx.seed(vec![
            task(&id(230, "a"), TaskStatus::Todo, 1, 1, None),
            task(&id(231, "b"), TaskStatus::Todo, 1, 1, None),
            task(&id(231, "c"), TaskStatus::Todo, 1, 1, None),
        ]);
        let fits = fx.list(&filter(), &request(Sort::Id, 1)).unwrap();
        let token = fits.next_cursor.unwrap();
        assert_eq!(token.as_str().len(), 512);
        // The next page would end on a 231-byte ID: refused whole, loudly.
        let error = fx
            .list(
                &filter(),
                &PageRequest {
                    cursor: Some(token.clone()),
                    ..request(Sort::Id, 1)
                },
            )
            .unwrap_err();
        assert!(
            matches!(error, SqliteError::Unrepresentable(_)),
            "{error:?}"
        );
        // A page that ends before the long IDs, or on the last row, needs no token.
        let rest = PageRequest {
            cursor: Some(token),
            ..request(Sort::Id, 2)
        };
        let last = fx.list(&filter(), &rest).unwrap();
        assert_eq!(last.items.len(), 2);
        assert!(last.next_cursor.is_none());
        fx.state.ensure_available().unwrap();
    });
}

/// Issue 265 evidence, not an assertion: what one page costs today, before any
/// tuning is proposed. Run with
/// `cargo test -p orch-infra --lib task_adapter::tests::record_page_costs -- --ignored --nocapture`.
/// The plans are of statements mirrored from `task_page::query` (its text is
/// built inline); the statement counts are of the real hydration path.
#[test]
#[ignore = "records query-plan and statement-count evidence for issue 265"]
fn record_page_costs() {
    use rusqlite::hooks::{AuthAction, Authorization};
    use std::sync::atomic::{AtomicU64, Ordering};
    let (_temp, data) = crate::sqlite::test_data();
    let disk = DiskBlobStore::open(data.root()).unwrap();
    // The schema the migrations build, in memory: the authorizer needs `&mut`.
    let mut conn = rusqlite::Connection::open_in_memory().unwrap();
    for schema in [
        migrations::INITIAL_SCHEMA,
        migrations::ATTENTION_FACTS,
        migrations::TASK_FORMAT,
    ] {
        conn.execute_batch(schema).unwrap();
    }
    let rank = "CASE status WHEN 'in_progress' THEN 0 WHEN 'retrying' THEN 1 WHEN 'review' THEN 2 WHEN 'todo' THEN 3 WHEN 'done' THEN 4 WHEN 'failed' THEN 5 WHEN 'cancelled' THEN 6 ELSE 7 END";
    let order: [(&str, String); 6] = [
        ("Default", format!("{rank}, updated_at DESC, position, id")),
        ("Id", "id".into()),
        ("Position", "position, id".into()),
        ("Priority", "priority, position, id".into()),
        ("CreatedAt", "created_at, position, id".into()),
        ("UpdatedAt", "updated_at DESC, position, id".into()),
    ];
    let plan = |conn: &rusqlite::Connection, sql: &str| -> Vec<String> {
        let mut statement = conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
        statement
            .query_map([], |r| r.get::<_, String>(3))
            .unwrap()
            .map(|row| row.unwrap())
            .collect()
    };
    let count = std::sync::Arc::new(AtomicU64::new(0));
    let seen = count.clone();
    conn.authorizer(Some(move |ctx: rusqlite::hooks::AuthContext<'_>| {
        if matches!(ctx.action, AuthAction::Select) {
            seen.fetch_add(1, Ordering::SeqCst);
        }
        Authorization::Allow
    }))
    .unwrap();
    let state = FailureState::new();
    let cursors = TaskCursors::new("generation-1");
    for rows in [1_usize, 10, 100] {
        let tx = conn.transaction().unwrap();
        {
            let caller = owner();
            let sql = TaskSql {
                tx: &tx,
                project: "p",
                state: state.clone(),
                source: FailureSource::Writer,
                disk: &disk,
            };
            let mut write = TaskWrite::new(sql, &caller);
            for n in 0..rows {
                let mut t = task(
                    &format!("e{rows}-{n:03}"),
                    TaskStatus::Todo,
                    1,
                    1,
                    Some("g"),
                );
                t.labels = vec!["l1".into(), "l2".into()];
                t.depends_on = vec!["d1".into()];
                write.create(t).unwrap();
            }
        }
        tx.commit().unwrap();
        let tx = conn.transaction().unwrap();
        let sql = TaskSql {
            tx: &tx,
            project: "p",
            state: state.clone(),
            source: FailureSource::Reader,
            disk: &disk,
        };
        let filter = Filter::Tasks {
            status: None,
            goal: Some("g".into()),
        };
        let before = count.load(Ordering::SeqCst);
        let started = std::time::Instant::now();
        let got = page(&sql, &cursors, 0, &filter, &request(Sort::Position, 1000)).unwrap();
        let elapsed = started.elapsed();
        let compiled = count.load(Ordering::SeqCst) - before;
        eprintln!(
            "page of {:4} records: {compiled:4} statement compiles, {:.1} per record, {elapsed:?} (debug build)",
            got.items.len(),
            compiled as f64 / got.items.len() as f64
        );
        tx.commit().unwrap();
    }
    eprintln!("--- plans after 111 rows (mirrored statements) ---");
    for (name, by) in &order {
        for (label, extra) in [("first", ""), ("filtered-todo", " AND status = 'todo'")] {
            let sql = format!(
                "SELECT id FROM tasks WHERE project_id = 'p'{extra} ORDER BY {by} LIMIT 101"
            );
            eprintln!("{name:9} {label:13} {:?}", plan(&conn, &sql));
        }
    }
    eprintln!(
        "position bounds       {:?}",
        plan(
            &conn,
            "SELECT MIN(position),MAX(position) FROM tasks WHERE project_id = 'p'"
        )
    );
}
