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
use sha2::{Digest, Sha256};
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
            // A walk that does not end is a failure, not a hung test.
            assert!(pages.len() < 10_000, "the walk does not terminate");
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
        let (next, following) = task_page_continuation(fx, &bound, Sort::Priority);
        let future = cursors
            .issue(
                "p",
                &bound,
                Sort::Priority,
                &next,
                &following,
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
) -> (task_page::Continuation, Vec<rusqlite::types::Value>) {
    fx.pool
        .read(Instant::now() + Duration::from_secs(15), |tx| {
            let page = task_page::query(tx, "p", filter, &request(sort, 2), None).unwrap();
            Ok((page.next.unwrap(), page.following.unwrap()))
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

/// How many bytes of ID a token carries whole: the 256 raw bytes, less the
/// 26-byte header and eight per integer key.
fn inline_capacity(sort: Sort) -> usize {
    256 - 26
        - 8 * match sort {
            Sort::Id => 0,
            Sort::Position => 1,
            Sort::Priority | Sort::CreatedAt | Sort::UpdatedAt => 2,
            _ => 3,
        }
}

/// An ID of exactly `len` bytes that no other `(len, tag)` shares.
fn id_of(len: usize, tag: &str) -> String {
    format!("{tag}{}", "k".repeat(len - tag.len()))
}

/// Valid stored IDs from one byte to the 65,536-byte limit. Every length at
/// and beside each sort's inline capacity; the longest; runs that share all but
/// their last bytes, at several lengths, so that only the digest tells them
/// apart; and multi-byte characters straddling each sort's prefix cut.
fn long_id_dataset() -> Vec<String> {
    let mut ids = vec!["a".to_owned(), "t-short".to_owned()];
    for sort in SORTS {
        let cap = inline_capacity(sort);
        for len in [cap - 1, cap, cap + 1] {
            ids.push(id_of(len, &format!("b{sort:?}")));
        }
        // A 4-byte character that starts 3, 2, 1 and 0 bytes inside the prefix's
        // room, so that it is cut off whole or fits.
        let room = cap - 20;
        for cut in 0..4 {
            ids.push(format!(
                "c{sort:?}{}\u{1D11E}{}",
                "m".repeat(room - 3 - 1 - format!("{sort:?}").len() + cut),
                "z".repeat(40)
            ));
        }
    }
    let shared = "s".repeat(400);
    for tail in ["1", "2", "3", "22"] {
        ids.push(format!("{shared}{tail}"));
    }
    let wide = "\u{20AC}".repeat(300);
    for tail in ["x", "y"] {
        ids.push(format!("{wide}{tail}"));
    }
    ids.push("q".repeat(65_535));
    ids.push(format!("{}a", "q".repeat(65_535)));
    ids.push(format!("{}b", "q".repeat(65_535)));
    ids.push(format!("{}c", "q".repeat(65_534)));
    ids
}

fn rank(status: &str) -> usize {
    [
        "in_progress",
        "retrying",
        "review",
        "todo",
        "done",
        "failed",
        "cancelled",
    ]
    .iter()
    .position(|s| *s == status)
    .unwrap_or(7)
}

#[derive(PartialEq, Eq, PartialOrd, Ord)]
enum Key {
    Int(i64),
    Text(String),
    Newest(std::cmp::Reverse<String>),
}

/// The order of `records` under `sort`, by the rules of the spec written out
/// here, never by SQL: the keys, then the ID's bytes.
fn expected_order(records: &[Record], sort: Sort) -> Vec<String> {
    let keys = |r: &Record| {
        let Entity::Task(t) = &r.entity else {
            unreachable!()
        };
        let position = Key::Int(r.position as i64);
        let id = Key::Text(t.id.clone());
        // Times compare as their canonical strings; `updated_at` is newest first.
        let updated = Key::Newest(std::cmp::Reverse(t.updated_at.clone()));
        match sort {
            Sort::Default => vec![
                Key::Int(rank(t.status.as_str()) as i64),
                updated,
                position,
                id,
            ],
            Sort::Id => vec![id],
            Sort::Position => vec![position, id],
            Sort::Priority => vec![Key::Int(t.priority), position, id],
            Sort::CreatedAt => vec![Key::Text(t.created_at.clone()), position, id],
            Sort::UpdatedAt => vec![updated, position, id],
            _ => unreachable!(),
        }
    };
    let mut keyed = records.iter().map(|r| (keys(r), r)).collect::<Vec<_>>();
    keyed.sort_by(|a, b| a.0.cmp(&b.0));
    keyed
        .iter()
        .map(|(_, r)| r.entity.id().to_owned())
        .collect()
}

/// The statuses, priorities and times of the dataset: varied, or all equal, so
/// that the ID alone orders the rows.
fn long_id_tasks(ids: &[String], tied: bool) -> Vec<Task> {
    use TaskStatus::*;
    let statuses = [Todo, InProgress, Done, Review];
    ids.iter()
        .enumerate()
        .map(|(i, id)| {
            if tied {
                task(id, Todo, 1, 1, None)
            } else {
                task(id, statuses[i % 4], (i % 3) as i64, (i % 5) as i64, None)
            }
        })
        .collect()
}

fn token_version(token: &Cursor) -> u8 {
    raw(token)[0]
}

#[test]
fn every_stored_id_pages_at_every_sort_with_a_token_of_at_most_512_bytes() {
    let ids = long_id_dataset();
    assert!(ids.iter().any(|id| id.len() == 65_536));
    {
        let mut all = ids.clone();
        all.sort();
        all.dedup();
        assert_eq!(all.len(), ids.len(), "the dataset's IDs are distinct");
    }
    for tied in [false, true] {
        scenario(2, |fx| {
            fx.seed(long_id_tasks(&ids, tied));
            if tied {
                // Equal on every key, the position included.
                submit(fx.handle, |tx| {
                    tx.execute("UPDATE tasks SET position = 7", [])?;
                    Ok(())
                })
                .unwrap();
            }
            for sort in SORTS {
                let all = fx.list(&filter(), &request(sort, 1000)).unwrap();
                assert!(all.next_cursor.is_none());
                assert_eq!(all.items.len(), ids.len());
                let want = expected_order(&all.items, sort);
                assert_eq!(Fixture::ids(&all), want, "{sort:?} tied={tied}");
                let mut references = 0;
                for limit in [1, 2, 3, 7] {
                    let pages = fx.walk(&filter(), request(sort, limit));
                    // Ordered, exactly once, whatever the page size.
                    let got: Vec<_> = pages.iter().flat_map(Fixture::ids).collect();
                    assert_eq!(got, want, "{sort:?} limit {limit} tied={tied}");
                    assert_eq!(pages.len(), ids.len().div_ceil(limit));
                    for (n, page) in pages.iter().enumerate() {
                        let last = n + 1 == pages.len();
                        assert_eq!(page.next_cursor.is_none(), last);
                        assert!(!page.items.is_empty());
                        assert!(last || page.items.len() == limit);
                        let Some(token) = &page.next_cursor else {
                            continue;
                        };
                        assert!(token.as_str().len() <= 512, "{sort:?} {limit}");
                        // Whole while it fits, by reference once it does not.
                        let id = Fixture::ids(page).pop().unwrap();
                        let want_version = if id.len() <= inline_capacity(sort) {
                            1
                        } else {
                            2
                        };
                        assert_eq!(token_version(token), want_version, "{sort:?} {}", id.len());
                        references += usize::from(want_version == 2);
                    }
                }
                assert!(references > 0);
            }
            fx.state.ensure_available().unwrap();
        });
    }
}

#[test]
fn a_walk_with_a_page_size_of_one_crosses_consecutive_ids_of_the_longest_length() {
    scenario(1, |fx| {
        let shared = "q".repeat(65_535);
        let ids = [
            format!("{shared}a"),
            format!("{shared}b"),
            format!("{shared}c"),
        ];
        fx.seed(
            ids.iter()
                .map(|id| task(id, TaskStatus::Todo, 1, 1, None))
                .collect(),
        );
        for sort in SORTS {
            let pages = fx.walk(&filter(), request(sort, 1));
            let got: Vec<_> = pages.iter().flat_map(Fixture::ids).collect();
            assert_eq!(got, ids, "{sort:?}");
            // The two nonterminal tokens are each a different position.
            let tokens: Vec<_> = pages.iter().filter_map(|p| p.next_cursor.clone()).collect();
            assert_eq!(tokens.len(), 2);
            assert_ne!(tokens[0], tokens[1]);
            assert!(tokens.iter().all(|t| t.as_str().len() <= 512));
        }
        fx.state.ensure_available().unwrap();
    });
}

/// The raw bytes of a reference token for a page that ends on a row of 65,536
/// bytes, and the offset in them where the reference begins.
fn reference_token(fx: &Fixture<'_>) -> (Vec<u8>, usize) {
    let shared = "q".repeat(65_535);
    fx.seed(vec![
        task(&format!("{shared}a"), TaskStatus::Todo, 1, 1, None),
        task(&format!("{shared}b"), TaskStatus::Todo, 1, 1, None),
        task(&format!("{shared}c"), TaskStatus::Todo, 1, 1, None),
    ]);
    let token = fx
        .list(&filter(), &request(Sort::Id, 1))
        .unwrap()
        .next_cursor
        .unwrap();
    let good = raw(&token);
    assert_eq!(good[0], 2);
    // Sort::Id has no integer key: the header, then the reference.
    (good, 26)
}

/// The fallback of a reference token starts 20 bytes in, after its length and
/// digest; of a `Successor` the next 20 bytes are the next row's; the prefix
/// follows.
const KIND: usize = 20;
const SUCCESSOR_PREFIX: usize = KIND + 1 + 20;

#[test]
fn a_reference_that_issue_could_not_have_written_is_refused() {
    scenario(1, |fx| {
        let (good, at) = reference_token(fx);
        let sort = Sort::Id;
        let try_raw = |bytes: &[u8]| fx.list(&filter(), &resumed(&cursor(hex(bytes)), sort));
        let shared = "q".repeat(65_535);
        let rest = [format!("{shared}b"), format!("{shared}c")];
        let page = try_raw(&good).unwrap();
        // Resumed after the first row, not at it.
        assert_eq!(Fixture::ids(&page), rest);
        assert_eq!(
            good[at + KIND],
            2,
            "the keys tie and the IDs share 65,535 bytes"
        );
        // The length: one that would have fit whole, and none at all, are not
        // references that `issue` writes.
        for length in [230u32, 0] {
            let mut bad = good.clone();
            bad[at..at + 4].copy_from_slice(&length.to_be_bytes());
            invalid_cursor(try_raw(&bad));
        }
        // A reference to a row whose ID fits whole: found and digest-true, yet
        // not a token `issue` writes, so not one position with two tokens.
        let fitting = id_of(inline_capacity(sort), "n");
        fx.seed(vec![task(&fitting, TaskStatus::Todo, 1, 1, None)]);
        let mut forged = good[..at].to_vec();
        forged.extend((fitting.len() as u32).to_be_bytes());
        forged.extend(&Sha256::digest(fitting.as_bytes())[..16]);
        forged.extend(&good[at + KIND..at + SUCCESSOR_PREFIX]);
        forged.extend(&fitting.as_bytes()[..good.len() - at - SUCCESSOR_PREFIX]);
        invalid_cursor(try_raw(&forged));
        // The same forgery in the form `issue` does write resumes.
        let mut whole = good[..at].to_vec();
        whole[0] = 1;
        whole.extend(fitting.as_bytes());
        try_raw(&whole).unwrap();
        // A length or digest that names no row is a row that is gone: the walk
        // goes on at its fallback, the next row, which is where it was.
        for index in [at + 2, at + 3, at + 4, at + 19] {
            let mut bad = good.clone();
            bad[index] ^= 1;
            assert_eq!(Fixture::ids(&try_raw(&bad).unwrap()), rest, "{index}");
        }
        // What the fallback names matters only once the row is gone.
        for index in [at + KIND + 4, at + KIND + 20] {
            let mut bad = good.clone();
            bad[index] ^= 1;
            assert_eq!(Fixture::ids(&try_raw(&bad).unwrap()), rest, "{index}");
            let mut gone = bad.clone();
            gone[at + 4] ^= 1;
            invalid_cursor(try_raw(&gone));
        }
        // A prefix byte that makes it another prefix finds neither row; one that
        // ends before the next 20 bytes of the fallback, or is cut off, or is
        // not UTF-8, or a kind that is none of the three, is no reference.
        for index in [at + SUCCESSOR_PREFIX, good.len() - 1] {
            let mut bad = good.clone();
            bad[index] ^= 1;
            invalid_cursor(try_raw(&bad));
        }
        let mut kind = good.clone();
        kind[at + KIND] = 3;
        invalid_cursor(try_raw(&kind));
        // A prefix a few bytes short still names the rows: one more byte of it
        // than `issue` could leave out, and it is not a prefix that fits.
        for short in 1..=3 {
            assert_eq!(
                Fixture::ids(&try_raw(&good[..good.len() - short]).unwrap()),
                rest
            );
        }
        invalid_cursor(try_raw(&good[..good.len() - 4]));
        let mut not_utf8 = good.clone();
        *not_utf8.last_mut().unwrap() = 0xff;
        invalid_cursor(try_raw(&not_utf8));
        invalid_cursor(try_raw(&good[..at + 19]));
        invalid_cursor(try_raw(&good[..at + KIND + 10]));
        // Neither the failures nor the lookups latch the pool.
        try_raw(&good).unwrap();
        fx.state.ensure_available().unwrap();
    });
}

#[test]
fn a_reference_to_a_deleted_or_updated_row_resumes() {
    scenario(1, |fx| {
        let tall = |tag: &str| format!("{}{tag}", "w".repeat(400));
        fx.seed(vec![
            task(&tall("1"), TaskStatus::Todo, 1, 1, None),
            task(&tall("2"), TaskStatus::Todo, 1, 2, None),
            task(&tall("3"), TaskStatus::Todo, 2, 3, None),
        ]);
        let head =
            |fx: &Fixture<'_>| fx.list(&filter(), &request(Sort::Id, 1)).unwrap().items[0].clone();
        for sort in [Sort::Id, Sort::Priority] {
            let first = fx.list(&filter(), &request(sort, 1)).unwrap();
            assert_eq!(Fixture::ids(&first), [tall("1")]);
            let token = first.next_cursor.unwrap();
            assert_eq!(token_version(&token), 2);
            // The row it ends on changes its sort key before the next page: the
            // walk goes on after its old position, as for a token that holds the ID.
            let Record {
                entity: Entity::Task(old),
                revision,
                ..
            } = head(fx)
            else {
                unreachable!()
            };
            let mut moved = (*old).clone();
            moved.priority = 9;
            fx.write(move |w| w.update(moved, revision).map(drop))
                .unwrap();
            let rest = fx.list(&filter(), &resumed(&token, sort)).unwrap();
            assert_eq!(Fixture::ids(&rest), [tall("2"), tall("3")], "{sort:?}");
            // Put it back for the next sort.
            let Record { revision, .. } = head(fx);
            fx.write(move |w| w.update(*old, revision).map(drop))
                .unwrap();
        }
        let token = (fx.list(&filter(), &request(Sort::Id, 1)).unwrap())
            .next_cursor
            .unwrap();
        let Record { revision, .. } = head(fx);
        fx.write(move |w| w.delete(&tall("1"), revision)).unwrap();
        // Deleted, the row it ends on is not found: the walk goes on at the next row
        // (issue 277), exactly once.
        let rest = fx.list(&filter(), &resumed(&token, Sort::Id)).unwrap();
        assert_eq!(Fixture::ids(&rest), [tall("2"), tall("3")]);
        assert!(rest.next_cursor.is_none());
        let again = fx.list(&filter(), &request(Sort::Id, 5)).unwrap();
        assert_eq!(Fixture::ids(&again), [tall("2"), tall("3")]);
        fx.state.ensure_available().unwrap();
    });
}

/// IDs for walks that delete the row each page ends on. Every one is long, or
/// is the short neighbour of a long one, and the long ones first differ at
/// every distance from the start, below and above where any sort cuts its
/// prefix: before it (a separator fits), after it (only the next row's reference
/// can tell them apart), in the middle of a multi-byte character, and where one
/// ID is a prefix of the next. Three of them are the longest stored ID and its
/// neighbours.
fn anchor_dataset() -> Vec<String> {
    let mut ids = vec!["a".to_owned(), "t-short".to_owned(), "r".repeat(150)];
    for (n, k) in [5, 100, 150, 160, 170, 178, 184, 190, 250, 399]
        .into_iter()
        .enumerate()
    {
        let letter = char::from(b'd' + n as u8);
        let stem = letter.to_string().repeat(k);
        for tail in ["a", "b", "c"] {
            ids.push(format!("{stem}{tail}{}", "x".repeat(400 - k)));
        }
    }
    let shared = "s".repeat(400);
    for tail in ["1", "2", "3"] {
        ids.push(format!("{shared}{tail}"));
    }
    // One ID is a prefix of the next two.
    let stem = "u".repeat(210);
    ids.push(stem.clone());
    ids.push(format!("{stem}{}", "x".repeat(190)));
    ids.push(format!("{stem}{}", "y".repeat(190)));
    // Neighbours that share the first bytes of a character and not its last.
    for (a, b) in [('\u{20AC}', '\u{20AD}'), ('\u{1D11E}', '\u{1D11F}')] {
        let stem = "\u{20AA}".repeat(100);
        ids.push(format!("{stem}{a}{}", "z".repeat(50)));
        ids.push(format!("{stem}{b}{}", "z".repeat(50)));
    }
    let longest = "q".repeat(65_535);
    for tail in ["a", "b", "c"] {
        ids.push(format!("{longest}{tail}"));
    }
    ids
}

/// Walk to the end with `limit`, deleting after each page the row it ends on,
/// or with `whole` every row of it, before the next page is read.
fn walk_deleting(fx: &Fixture<'_>, sort: Sort, limit: usize, whole: bool) -> Vec<String> {
    let mut request = request(sort, limit);
    let mut got = Vec::new();
    loop {
        let page = fx.list(&filter(), &request).unwrap();
        got.extend(Fixture::ids(&page));
        let doomed = if whole {
            page.items.clone()
        } else {
            page.items.last().cloned().into_iter().collect()
        };
        for record in doomed {
            let (id, revision) = (record.entity.id().to_owned(), record.revision);
            fx.write(move |w| w.delete(&id, revision)).unwrap();
        }
        request.cursor = page.next_cursor;
        if request.cursor.is_none() {
            return got;
        }
        assert!(got.len() < 10_000, "the walk does not terminate");
    }
}

#[test]
fn a_walk_returns_every_row_once_when_the_row_each_page_ends_on_is_deleted() {
    let ids = anchor_dataset();
    {
        let mut all = ids.clone();
        all.sort();
        all.dedup();
        assert_eq!(all.len(), ids.len(), "the dataset's IDs are distinct");
    }
    for sort in SORTS {
        for tied in [false, true] {
            for (limit, whole) in [(1, false), (2, false), (3, true)] {
                scenario(1, |fx| {
                    fx.seed(long_id_tasks(&ids, tied));
                    if tied {
                        // Equal on every key, the position included.
                        submit(fx.handle, |tx| {
                            tx.execute("UPDATE tasks SET position = 7", [])?;
                            Ok(())
                        })
                        .unwrap();
                    }
                    let all = fx.list(&filter(), &request(sort, 1000)).unwrap();
                    let want = expected_order(&all.items, sort);
                    let got = walk_deleting(fx, sort, limit, whole);
                    assert_eq!(
                        got, want,
                        "{sort:?} tied={tied} limit {limit} whole={whole}"
                    );
                    fx.state.ensure_available().unwrap();
                });
            }
        }
    }
}

/// Which fallback a reference token names: after the header, the integer keys
/// of `sort`, and the length and digest of the ID.
fn fallback_kind(token: &Cursor, sort: Sort) -> u8 {
    let keys = match sort {
        Sort::Id => 0,
        Sort::Position => 1,
        Sort::Priority | Sort::CreatedAt | Sort::UpdatedAt => 2,
        _ => 3,
    };
    raw(token)[26 + keys * 8 + 20]
}

fn delete_rows(fx: &Fixture<'_>, ids: &[&str]) {
    let all = fx.list(&filter(), &request(Sort::Id, 1000)).unwrap();
    for id in ids {
        let revision = all
            .items
            .iter()
            .find(|r| r.entity.id() == *id)
            .map(|r| r.revision)
            .expect("the row to delete exists");
        let id = (*id).to_owned();
        fx.write(move |w| w.delete(&id, revision)).unwrap();
    }
}

#[test]
fn a_deleted_row_is_resumed_at_the_shortest_fallback_and_without_one_it_is_refused() {
    scenario(1, |fx| {
        let near = |tag: &str| format!("{}{tag}{}", "v".repeat(100), "x".repeat(300));
        let far = |tag: &str| format!("{}{tag}", "w".repeat(400));
        let ids = [
            near("a"),
            near("b"),
            near("c"),
            far("1"),
            far("2"),
            far("3"),
        ];
        fx.seed(
            ids.iter()
                .enumerate()
                .map(|(i, id)| task(id, TaskStatus::Todo, i as i64, 1, None))
                .collect(),
        );
        let token_after = |sort: Sort, id: &str| {
            let mut request = request(sort, 1);
            loop {
                let page = fx.list(&filter(), &request).unwrap();
                if Fixture::ids(&page) == [id] {
                    return page.next_cursor.unwrap();
                }
                request.cursor = Some(page.next_cursor.expect("the walk reaches the row"));
            }
        };
        let resumed_ids = |token: &Cursor, sort: Sort| {
            let rest = PageRequest {
                cursor: Some(token.clone()),
                ..request(sort, 100)
            };
            Fixture::ids(&fx.list(&filter(), &rest).unwrap())
        };
        let from = |ids: &[String]| ids.to_vec();

        // The keys tie and the IDs part at byte 100, within what the token
        // carries of the first: the next row's own bytes up to there are the
        // fallback, and the row after it is found without the next row.
        let token = token_after(Sort::Id, &near("a"));
        assert_eq!(fallback_kind(&token, Sort::Id), 1);
        delete_rows(fx, &[&near("a"), &near("b")]);
        assert_eq!(resumed_ids(&token, Sort::Id), from(&ids[2..]));
        // Parting at byte 0 is the same fallback.
        let token = token_after(Sort::Id, &near("c"));
        assert_eq!(fallback_kind(&token, Sort::Id), 1);
        delete_rows(fx, &[&near("c")]);
        assert_eq!(resumed_ids(&token, Sort::Id), from(&ids[3..]));

        // The keys tie and the IDs part at byte 400: past the prefix. The next
        // row is the fallback.
        let token = token_after(Sort::Id, &far("1"));
        assert_eq!(fallback_kind(&token, Sort::Id), 2);
        delete_rows(fx, &[&far("1")]);
        assert_eq!(resumed_ids(&token, Sort::Id), from(&ids[4..]));
        // Both gone, no surviving row says where the first was among the rows that
        // share its prefix. The token is refused, not resumed at a guess, and
        // nothing else is the worse for it.
        delete_rows(fx, &[&far("2")]);
        invalid_cursor(fx.list(&filter(), &resumed(&token, Sort::Id)));
        assert_eq!(
            Fixture::ids(&fx.list(&filter(), &request(Sort::Id, 9)).unwrap()),
            from(&ids[5..])
        );
        fx.state.ensure_available().unwrap();
    });
    // Another key is the next row's, whatever the IDs: nothing of the ID is
    // needed, and the next row may go too.
    scenario(1, |fx| {
        let far = |tag: &str| format!("{}{tag}", "w".repeat(400));
        fx.seed(vec![
            task(&far("1"), TaskStatus::Todo, 1, 1, None),
            task(&far("2"), TaskStatus::Todo, 2, 1, None),
            task(&far("3"), TaskStatus::Todo, 3, 1, None),
        ]);
        let first = fx.list(&filter(), &request(Sort::Priority, 1)).unwrap();
        let token = first.next_cursor.unwrap();
        assert_eq!(fallback_kind(&token, Sort::Priority), 0);
        delete_rows(fx, &[&far("1"), &far("2")]);
        let rest = fx
            .list(&filter(), &resumed(&token, Sort::Priority))
            .unwrap();
        assert_eq!(Fixture::ids(&rest), [far("3")]);
        assert!(rest.next_cursor.is_none());
        // Past the keys of the row that is gone, not past the row: the same token
        // for the other sorts is refused as for any other sort.
        invalid_cursor(fx.list(&filter(), &resumed(&token, Sort::Id)));
        fx.state.ensure_available().unwrap();
    });
}

/// A `From` that `issue` could not have written is refused before any row is
/// looked up: not once the row is found to be gone, and not after it is found.
/// Its tail is the rest of one character, and the prefix is what the room leaves
/// after it, so a longer tail could only spend the prefix, and a scan with it,
/// on nothing (a tail of 206 bytes leaves none for the sort by ID).
#[test]
fn a_from_fallback_that_issue_could_not_have_written_is_refused_before_any_lookup() {
    scenario(1, |fx| {
        let near = |tag: &str| format!("{}{tag}{}", "v".repeat(100), "x".repeat(300));
        let ids = [near("a"), near("b")];
        fx.seed(
            ids.iter()
                .map(|id| task(id, TaskStatus::Todo, 1, 1, None))
                .collect(),
        );
        let sort = Sort::Id;
        let token = fx.list(&filter(), &request(sort, 1)).unwrap().next_cursor;
        let good = raw(&token.unwrap());
        // After the header, the length and digest, then the fallback: the kind,
        // `k`, the tail's length, the tail and the prefix.
        let at = 26 + KIND;
        assert_eq!(good[at], 1, "the IDs part at byte 100: a `From`");
        let tail_len = usize::from(good[at + 3]);
        assert_eq!(tail_len, 1);
        let (tail, prefix) = good[at + 4..].split_at(tail_len);
        let forge = |k: u16, tail: &[u8], prefix: &[u8]| {
            let mut bytes = good[..at].to_vec();
            bytes.push(1);
            bytes.extend(k.to_be_bytes());
            bytes.push(tail.len() as u8);
            bytes.extend(tail);
            bytes.extend(prefix);
            bytes
        };
        let try_raw = |bytes: &[u8]| fx.list(&filter(), &resumed(&cursor(hex(bytes)), sort));
        // The anchor is there, so each of these would resume after it, were the
        // shape looked at only once the anchor is gone.
        assert_eq!(Fixture::ids(&try_raw(&good).unwrap()), [near("b")]);
        assert_eq!(
            Fixture::ids(&try_raw(&forge(100, tail, prefix)).unwrap()),
            [near("b")]
        );
        // No tail, and one a character cannot have.
        invalid_cursor(try_raw(&forge(100, &[], prefix)));
        // (The prefix is cut to the room that tail leaves, so that the length
        // of the tail is the only fault.)
        invalid_cursor(try_raw(&forge(100, b"xxxxx", &prefix[..prefix.len() - 4])));
        // A tail that takes all the room: the empty prefix is every ID's, so the
        // scan for the anchor would be over every row of the project.
        let wide = [b'x'; 206];
        let all = forge(0, &wide, &[]);
        assert_eq!(all.len(), 256, "it fits the token");
        invalid_cursor(try_raw(&all));
        // `k` past the prefix, and bytes that are not UTF-8 once joined.
        invalid_cursor(try_raw(&forge(prefix.len() as u16 + 1, tail, prefix)));
        invalid_cursor(try_raw(&forge(100, &[0xff], prefix)));
        invalid_cursor(try_raw(&forge(99, &[0xa9], prefix)));
        // The same with the anchor gone.
        delete_rows(fx, &[&ids[0]]);
        assert_eq!(Fixture::ids(&try_raw(&good).unwrap()), [near("b")]);
        invalid_cursor(try_raw(&forge(100, &[], prefix)));
        invalid_cursor(try_raw(&all));
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

#[test]
fn a_reference_token_for_another_sort_is_refused() {
    scenario(1, |fx| {
        let (good, _) = reference_token(fx);
        // Sort::Id's token under every other sort.
        for sort in SORTS.into_iter().filter(|s| *s != Sort::Id) {
            invalid_cursor(fx.list(&filter(), &resumed(&cursor(hex(&good)), sort)));
        }
        fx.state.ensure_available().unwrap();
    });
}
