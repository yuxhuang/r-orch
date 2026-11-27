use super::*;
use orch_core::store::{Principal, Storage, StorageExt};

fn owner() -> Caller {
    Caller {
        project: "p".into(),
        principal: Principal::Owner {
            identity: "synthetic".into(),
        },
    }
}
fn team(id: &str) -> Entity {
    Entity::Team(Box::new(serde_json::from_value(serde_json::json!({"id":id,"name":id,"status":"active","members":[],"task_pool":[],"lead_agent_id":"a","created_at":"opaque","updated_at":"opaque","config":{"auto_claim":false}})).unwrap()))
}
#[tokio::test]
async fn owned_snapshot_pins_at_open_and_stops_with_last_storage_handle() {
    let (_temp, data) = crate::sqlite::test_data();
    let storage = SqliteStorage::open(Arc::new(data), StorageOptions::default()).unwrap();
    let snapshot = storage.read(owner()).await.unwrap();
    storage
        .transaction(owner(), orch_core::store::Lane::Entities, |u| {
            u.create(team("later"))
        })
        .await
        .unwrap();
    assert!(
        snapshot
            .get(EntityKind::Team, "later")
            .await
            .unwrap()
            .is_none()
    );
    let newer = storage.read(owner()).await.unwrap();
    assert!(
        newer
            .get(EntityKind::Team, "later")
            .await
            .unwrap()
            .is_some()
    );
    drop(storage);
    assert!(snapshot.get(EntityKind::Team, "later").await.is_err());
}
#[tokio::test]
async fn callback_panic_is_resumed_and_the_following_write_commits() {
    let (_temp, data) = crate::sqlite::test_data();
    let storage = SqliteStorage::open(Arc::new(data), StorageOptions::default()).unwrap();
    let other = storage.clone();
    let result = tokio::spawn(async move {
        other
            .write(
                owner(),
                orch_core::store::Lane::Entities,
                Box::new(|u| {
                    u.create(team("rolled-back"))?;
                    panic!("synthetic callback panic")
                }),
            )
            .await
    })
    .await;
    assert!(result.unwrap_err().is_panic());
    storage
        .transaction(owner(), orch_core::store::Lane::Entities, |u| {
            u.create(team("kept"))
        })
        .await
        .unwrap();
    let snapshot = storage.read(owner()).await.unwrap();
    assert!(
        snapshot
            .get(EntityKind::Team, "rolled-back")
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        snapshot
            .get(EntityKind::Team, "kept")
            .await
            .unwrap()
            .is_some()
    );
    assert!(storage.failure_state().failure().is_none());
    drop(snapshot);
    storage.close().await.unwrap();
}

#[tokio::test]
async fn idle_snapshot_releases_the_only_reader_and_reports_expiry() {
    let (_temp, data) = crate::sqlite::test_data();
    let storage = SqliteStorage::open(
        Arc::new(data),
        StorageOptions {
            read_connections: 1,
            snapshot_idle: Duration::from_millis(30),
            ..Default::default()
        },
    )
    .unwrap();
    let expired = storage.read(owner()).await.unwrap();
    // Opening this snapshot waits for the first worker's idle bound. There is
    // no sleep or unrelated await while an active snapshot is intentionally held.
    let next = storage.read(owner()).await.unwrap();
    assert!(
        expired
            .get(EntityKind::Team, "absent")
            .await
            .unwrap_err()
            .to_string()
            .contains("snapshot_expired")
    );
    assert!(
        next.get(EntityKind::Team, "absent")
            .await
            .unwrap()
            .is_none()
    );
    storage.close().await.unwrap();
}

#[tokio::test]
async fn fatal_reader_observation_discards_the_straddling_snapshot_only() {
    let (_temp, data) = crate::sqlite::test_data();
    let storage = SqliteStorage::open(Arc::new(data), StorageOptions::default()).unwrap();
    let snapshot = storage.read(owner()).await.unwrap();
    assert!(
        snapshot
            .get(EntityKind::Team, "absent")
            .await
            .unwrap()
            .is_none()
    );
    let error = SqliteError::Sql(rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CORRUPT),
        None,
    ));
    storage
        .failure_state()
        .observe(FailureSource::Reader, &error);
    assert!(snapshot.get(EntityKind::Team, "absent").await.is_err());
    // A read requested after the observation is served (issue 389); writes are not.
    let later = storage.read(owner()).await.unwrap();
    assert!(
        later
            .get(EntityKind::Team, "absent")
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        storage
            .transaction(owner(), orch_core::store::Lane::Entities, |u| {
                u.create(team("refused"))
            })
            .await
            .is_err()
    );
    storage.close().await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn close_ends_a_live_snapshot_and_releases_the_directory_lock() {
    use crate::data_dir::{DataDir, Holder, LockKind};
    let (temp, data) = crate::sqlite::test_data();
    let storage = SqliteStorage::open(Arc::new(data), StorageOptions::default()).unwrap();
    let snapshot = storage.read(owner()).await.unwrap();
    storage.close().await.unwrap();
    assert!(snapshot.get(EntityKind::Team, "absent").await.is_err());
    let second = DataDir::acquire(
        &temp.path().join("data"),
        &Holder {
            kind: LockKind::Daemon,
            pid: std::process::id(),
            started_at: "synthetic second".into(),
            binary_version: "test".into(),
            executable: "/synthetic/orch".into(),
        },
    )
    .unwrap();
    drop(second);
}

#[tokio::test]
async fn layout_order_and_leaf_pages_agree_for_every_non_task_shape() {
    use super::super::non_task_cursor::Layout;
    use orch_core::store::{ConfigEntry, Sort, TemplateEntry};
    use orch_core::test_support::{make_agent, make_run};
    use orch_domain::{Goal, GoalStatus};
    let (_temp, data) = crate::sqlite::test_data();
    let storage = SqliteStorage::open(Arc::new(data), StorageOptions::default()).unwrap();
    storage.transaction(owner(),store::Lane::Entities,|u| {
        for (index,id,name) in [(0,"b","é"),(1,"a","a"),(2,"c","a")] {
            let mut agent=make_agent(); agent.id=id.into();agent.name=name.into();
            u.create(Entity::Agent(Box::new(agent)))?;
            let Entity::Team(mut t)=team(id) else { unreachable!() };t.name=name.into();t.created_at=name.into();
            u.create(Entity::Team(t))?;
            let mut run=make_run();run.id=id.into();run.task_id="t".into();run.agent_id="a".into();run.attempt=[2,10,10][index];run.started_at=name.into();
            u.create(Entity::Run(Box::new(run)))?;
            u.create(Entity::Message(Box::new(serde_json::from_value(serde_json::json!({"id":id,"channel":"direct","from_agent_id":"sender","to_agent_id":"recipient","subject":"s","body":"b","created_at":name,"status":"pending"})).unwrap())))?;
            u.create(Entity::Goal(Box::new(Goal { id:id.into(),title:"title".into(),description:"description".into(),status:[GoalStatus::Active,GoalStatus::Paused,GoalStatus::Active][index],assignee:None,created_at:name.into(),updated_at:[None,Some(""),Some("z")][index].map(str::to_owned) })))?;
            u.create(Entity::Context(orch_core::storage::ContextEntry { key:id.into(),value:"v".into(),created_at:"opaque".into(),updated_at:"opaque".into(),ttl_ms:None,expires_at:None }))?;
            u.create(Entity::Config(ConfigEntry { key:id.into(),value:"v".into() }))?;
            u.create(Entity::Template(TemplateEntry { name:id.into(),body:"v".into() }))?;
        }
        Ok(())
    }).await.unwrap();
    let snapshot = storage.read(owner()).await.unwrap();
    for filter in [
        Filter::Agents,
        Filter::Teams,
        Filter::Runs {
            task: None,
            agent: None,
        },
        Filter::Runs {
            task: Some("t".into()),
            agent: Some("a".into()),
        },
        Filter::Messages {
            recipient: Some("recipient".into()),
        },
        Filter::PendingMessages {
            recipient: "recipient".into(),
            now: "z".into(),
        },
        Filter::Goals { status: None },
        Filter::Context,
        Filter::Config,
        Filter::Templates,
    ] {
        for sort in [
            Sort::Default,
            Sort::Id,
            Sort::Position,
            Sort::Name,
            Sort::Priority,
            Sort::CreatedAt,
            Sort::UpdatedAt,
            Sort::Attempt,
        ] {
            let Some(layout) = Layout::of(&filter, sort) else {
                continue;
            };
            let request = PageRequest {
                limit: 1000,
                sort,
                with_count: true,
                ..Default::default()
            };
            let all = snapshot.list(&filter, &request).await.unwrap();
            assert_eq!(all.items.len(), 3, "{filter:?} {sort:?}");
            for rows in all.items.windows(2) {
                let a = page::tuple(&filter, sort, &rows[0]).unwrap();
                let b = page::tuple(&filter, sort, &rows[1]).unwrap();
                assert!(layout.compare(&a, &b).is_lt(), "{filter:?} {sort:?}");
            }
            let mut request = PageRequest {
                limit: 1,
                ..request
            };
            let mut walked = Vec::new();
            loop {
                let one = snapshot.list(&filter, &request).await.unwrap();
                walked.extend(one.items);
                let Some(cursor) = one.next_cursor else { break };
                request.cursor = Some(cursor);
            }
            assert_eq!(walked, all.items, "{filter:?} {sort:?}");
        }
    }
    storage.close().await.unwrap();
}

#[tokio::test]
async fn a_queued_snapshot_is_refused_at_admission_deadline() {
    let (_temp, data) = crate::sqlite::test_data();
    let storage = SqliteStorage::open(
        Arc::new(data),
        StorageOptions {
            read_connections: 1,
            ..Default::default()
        },
    )
    .unwrap();
    let held = storage.read(owner()).await.unwrap();
    let refused = tokio::time::timeout(Duration::from_millis(500), storage.read(owner())).await;
    assert!(
        matches!(refused,Ok(Err(ref e)) if e.to_string().contains("overloaded")),
        "queued read must answer overload before the holder's idle bound"
    );
    assert!(
        held.get(EntityKind::Team, "missing")
            .await
            .unwrap()
            .is_none()
    );
    storage.close().await.unwrap();
}

// Exercise the real admission and worker path while supplying deterministic
// adapter jobs, without test hooks in the production worker.
async fn worker_snapshot(storage: &SqliteStorage) -> Snapshot {
    let (commands, receiver) = mpsc::sync_channel(16);
    let (ready, answer) = oneshot::channel();
    let observed = storage.failure_state().fatal_observations();
    let mut request = OpenRead {
        caller: owner(),
        deadline: Instant::now() + Duration::from_secs(2),
        commands: receiver,
        ready,
    };
    loop {
        match storage.owner.reads.try_send(request) {
            Ok(()) => break,
            Err(mpsc::TrySendError::Full(queued)) => {
                assert!(Instant::now() < queued.deadline, "test admission deadline");
                request = queued;
                tokio::task::yield_now().await;
            }
            Err(_) => panic!("test reader closed"),
        }
    }
    answer.await.unwrap().unwrap();
    Snapshot {
        owner: storage.owner.clone(),
        caller: owner(),
        commands,
        capacity: Arc::new(Semaphore::new(16)),
        observed,
    }
}

#[tokio::test]
async fn reader_operation_panic_retires_view_without_losing_worker() {
    let (_temp, data) = crate::sqlite::test_data();
    let storage = SqliteStorage::open(
        Arc::new(data),
        StorageOptions {
            read_connections: 1,
            ..Default::default()
        },
    )
    .unwrap();
    let snapshot = worker_snapshot(&storage).await;
    let error = snapshot
        .call::<()>(|_| panic!("synthetic reader panic"))
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("reader operation panicked"),
        "{error}"
    );
    assert!(snapshot.get(EntityKind::Team, "absent").await.is_err());
    let next = worker_snapshot(&storage).await;
    assert!(
        next.get(EntityKind::Team, "absent")
            .await
            .unwrap()
            .is_none()
    );
    assert!(storage.failure_state().failure().is_none());
    drop(next);
    storage.close().await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn forty_snapshot_calls_wait_for_bounded_capacity() {
    let (_temp, data) = crate::sqlite::test_data();
    let storage = SqliteStorage::open(Arc::new(data), StorageOptions::default()).unwrap();
    let snapshot = Arc::new(worker_snapshot(&storage).await);
    let (release, blocked) = mpsc::sync_channel(1);
    let (entered, started) = oneshot::channel();
    let first = snapshot.clone();
    let blocker = tokio::spawn(async move {
        first
            .call(move |_| {
                let _ = entered.send(());
                blocked.recv().unwrap();
                Ok(())
            })
            .await
    });
    started.await.unwrap();
    let mut calls = Vec::new();
    for _ in 0..40 {
        let snapshot = snapshot.clone();
        calls.push(tokio::spawn(async move {
            snapshot.get(EntityKind::Config, "absent").await
        }));
        tokio::task::yield_now().await;
    }
    // The runtime remained able to poll this task while the bounded queue was full.
    release.send(()).unwrap();
    blocker.await.unwrap().unwrap();
    let mut failures = Vec::new();
    for call in calls {
        if let Err(e) = call.await.unwrap() {
            failures.push(e.to_string());
        }
    }
    assert!(failures.is_empty(), "{failures:?}");
    storage.close().await.unwrap();
}

fn corrupt_error() -> SqliteError {
    SqliteError::Sql(rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CORRUPT),
        None,
    ))
}
#[tokio::test]
async fn first_fatal_read_replies_unavailable_after_observation() {
    let (_temp, data) = crate::sqlite::test_data();
    let storage = SqliteStorage::open(Arc::new(data), StorageOptions::default()).unwrap();
    let snapshot = worker_snapshot(&storage).await;
    let error = snapshot
        .call::<()>(|_| Err(corrupt_error()))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("storage_unavailable"), "{error}");
    let failure = storage.failure_state().failure().unwrap();
    assert_eq!(failure.source, FailureSource::Reader);
    assert_eq!(
        failure.causes,
        vec![(
            super::super::Role::Operation,
            super::super::FatalCause::SqliteCorrupt
        )]
    );
    storage.close().await.unwrap();
}
#[tokio::test]
async fn fatal_observation_during_successful_read_prevents_reply() {
    let (_temp, data) = crate::sqlite::test_data();
    let storage = SqliteStorage::open(Arc::new(data), StorageOptions::default()).unwrap();
    let snapshot = worker_snapshot(&storage).await;
    let state = storage.failure_state();
    let error = snapshot
        .call(move |_| {
            // Another connection publishes the fatal observation after admission,
            // before this successful operation can expose its value.
            std::thread::spawn(move || state.observe(FailureSource::Writer, &corrupt_error()))
                .join()
                .unwrap();
            Ok("must not escape")
        })
        .await
        .unwrap_err();
    assert!(error.to_string().contains("storage_unavailable"), "{error}");
    storage.close().await.unwrap();
}

#[tokio::test]
async fn ordinary_sql_error_retires_connection_and_counts_once() {
    let (_temp, data) = crate::sqlite::test_data();
    let storage = SqliteStorage::open(
        Arc::new(data),
        StorageOptions {
            read_connections: 1,
            ..Default::default()
        },
    )
    .unwrap();
    let snapshot = worker_snapshot(&storage).await;
    snapshot
        .call(|v| {
            v.tx.execute_batch("PRAGMA cache_size=1234")?;
            Ok(())
        })
        .await
        .unwrap();
    let result = snapshot
        .call::<()>(|_| {
            Err(SqliteError::Sql(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_IOERR),
                None,
            )))
        })
        .await;
    assert!(result.is_err());
    assert_eq!(storage.failure_state().pool_failure_streak(), 1);
    let next = worker_snapshot(&storage).await;
    let size = next
        .call(|v| {
            Ok(v.tx.query_row("PRAGMA cache_size", [], |r| r.get::<_, i64>(0))?)
        })
        .await
        .unwrap();
    assert_ne!(size, 1234, "failed connection must be reopened");
    assert_eq!(
        storage.failure_state().pool_failure_streak(),
        1,
        "an open snapshot is not a finished pool read"
    );
    storage.close().await.unwrap();
}

#[tokio::test]
async fn invalid_cursor_keeps_snapshot_and_failure_streak() {
    let (_temp, data) = crate::sqlite::test_data();
    let storage = SqliteStorage::open(Arc::new(data), StorageOptions::default()).unwrap();
    let snapshot = storage.read(owner()).await.unwrap();
    let result = snapshot
        .list(
            &Filter::Teams,
            &PageRequest {
                cursor: Some(store::Cursor::new("malformed".into()).unwrap()),
                ..Default::default()
            },
        )
        .await;
    assert!(matches!(result, Err(StoreError::InvalidCursor)));
    assert!(
        snapshot
            .get(EntityKind::Config, "absent")
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(storage.failure_state().pool_failure_streak(), 0);
    storage.close().await.unwrap();
}

#[tokio::test]
async fn cancelled_and_expired_opens_do_not_retire_a_healthy_connection() {
    for cancelled in [true, false] {
        let (_temp, data) = crate::sqlite::test_data();
        let storage = SqliteStorage::open(
            Arc::new(data),
            StorageOptions {
                read_connections: 1,
                ..Default::default()
            },
        )
        .unwrap();
        let held = worker_snapshot(&storage).await;
        held.call(|v| {
            v.tx.execute_batch("PRAGMA cache_size=1234")?;
            Ok(())
        })
        .await
        .unwrap();
        let (_commands, receiver) = mpsc::sync_channel(16);
        let (ready, answer) = oneshot::channel();
        let deadline = if cancelled {
            Instant::now() + Duration::from_secs(5)
        } else {
            Instant::now()
        };
        storage
            .owner
            .reads
            .try_send(OpenRead {
                caller: owner(),
                deadline,
                commands: receiver,
                ready,
            })
            .unwrap_or_else(|_| panic!("stale admission"));
        if cancelled {
            drop(answer);
        } else {
            drop(held);
            assert!(matches!(
                answer.await.unwrap(),
                Err(SqliteError::Overloaded)
            ));
            let next = worker_snapshot(&storage).await;
            let size = next
                .call(|v| {
                    Ok(v.tx.query_row("PRAGMA cache_size", [], |r| r.get::<_, i64>(0))?)
                })
                .await
                .unwrap();
            assert_eq!(size, 1234, "expired open must skip the pool");
            storage.close().await.unwrap();
            continue;
        }
        drop(held);
        let next = worker_snapshot(&storage).await;
        let size = next
            .call(|v| {
                Ok(v.tx.query_row("PRAGMA cache_size", [], |r| r.get::<_, i64>(0))?)
            })
            .await
            .unwrap();
        assert_eq!(size, 1234, "cancelled open must skip the pool");
        storage.close().await.unwrap();
    }
}

#[tokio::test(flavor = "current_thread")]
async fn close_stops_idle_snapshot_independently_of_its_long_idle_bound() {
    let (_temp, data) = crate::sqlite::test_data();
    let storage = SqliteStorage::open(
        Arc::new(data),
        StorageOptions {
            snapshot_idle: Duration::from_secs(60),
            ..Default::default()
        },
    )
    .unwrap();
    let snapshot = storage.read(owner()).await.unwrap();
    let close = storage.clone();
    let mut closing = tokio::spawn(async move { close.close().await });
    let stopped = tokio::time::timeout(Duration::from_secs(1), &mut closing).await;
    // Also let a mutated worker exit on channel disconnection before asserting.
    drop(snapshot);
    match stopped {
        Ok(result) => result.unwrap().unwrap(),
        Err(_) => {
            closing.await.unwrap().unwrap();
            panic!("close waited for idle expiry");
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn close_interrupts_a_statement_that_is_already_running() {
    let (_temp, data) = crate::sqlite::test_data();
    let (entered, running) = oneshot::channel();
    let entered = Mutex::new(Some(entered));
    let storage = SqliteStorage::open(
        Arc::new(data),
        StorageOptions {
            read_connections: 1,
            reader_setup: Some(Box::new(move |conn| {
                let mut entered = entered.lock().unwrap().take();
                let mut fallback = None;
                conn.progress_handler(
                    1000,
                    Some(move || {
                        if let Some(entered) = entered.take() {
                            fallback = Some(Instant::now() + Duration::from_secs(4));
                            let _ = entered.send(());
                        }
                        // A broken interrupt mutant still exits and cleans up.
                        fallback.is_some_and(|deadline| Instant::now() >= deadline)
                    }),
                )
                .unwrap();
            })),
            ..Default::default()
        },
    )
    .unwrap();
    let snapshot = worker_snapshot(&storage).await;
    let query = tokio::spawn(async move {
        snapshot.call(move |v| {
            v.tx.query_row("WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<1000000000) SELECT sum(x) FROM n", [], |r| r.get::<_, i64>(0)).map_err(Into::into)
        }).await
    });
    running.await.unwrap();
    let stopped = tokio::time::timeout(Duration::from_secs(1), storage.close()).await;
    let result = query.await.unwrap();
    storage.close().await.unwrap();
    assert!(result.is_err(), "close must interrupt SQL");
    stopped
        .expect("close must not wait for the statement fallback")
        .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn dropping_an_in_flight_snapshot_returns_its_connection_without_a_transaction() {
    let (_temp, data) = crate::sqlite::test_data();
    let storage = SqliteStorage::open(
        Arc::new(data),
        StorageOptions {
            read_connections: 1,
            snapshot_idle: Duration::from_secs(60),
            ..Default::default()
        },
    )
    .unwrap();
    let snapshot = worker_snapshot(&storage).await;
    let (release, blocked) = mpsc::sync_channel(1);
    let (entered, running) = oneshot::channel();
    let call = tokio::spawn(async move {
        snapshot
            .call(move |_| {
                let _ = entered.send(());
                blocked.recv().unwrap();
                Ok(())
            })
            .await
    });
    running.await.unwrap();
    call.abort();
    assert!(call.await.unwrap_err().is_cancelled());
    release.send(()).unwrap();
    let next = worker_snapshot(&storage).await;
    // A write after cancelled delivery must be visible in a new snapshot.
    drop(next);
    storage
        .transaction(owner(), store::Lane::Entities, |u| {
            u.create(team("after-cancel"))
        })
        .await
        .unwrap();
    let next = storage.read(owner()).await.unwrap();
    assert!(
        next.get(EntityKind::Team, "after-cancel")
            .await
            .unwrap()
            .is_some()
    );
    storage.close().await.unwrap();
}

#[tokio::test]
async fn failed_cleanup_prevents_native_list_and_commit_even_when_callback_swallows_it() {
    use orch_core::{store::ConfigEntry, test_support::make_task};
    let (_temp, data) = crate::sqlite::test_data();
    let data = Arc::new(data);
    let storage = SqliteStorage::open(data.clone(), StorageOptions::default()).unwrap();
    storage
        .transaction(owner(), store::Lane::Entities, |u| {
            let mut task = make_task();
            task.created_at = "2025-01-01T00:00:00.000Z".into();
            task.updated_at = task.created_at.clone();
            u.create(Entity::Task(Box::new(task)))?;
            u.create(Entity::Config(ConfigEntry {
                key: "kept".into(),
                value: "v".into(),
            }))?;
            Ok(())
        })
        .await
        .unwrap();
    super::super::open_connection(&data, ConnectionRole::Writer, Synchronous::Normal).unwrap().execute_batch(
        "CREATE TRIGGER abort_move BEFORE UPDATE ON tasks BEGIN SELECT RAISE(ROLLBACK, 'synthetic cleanup failure'); END"
    ).unwrap();
    let refused = Arc::new(AtomicBool::new(false));
    let seen = refused.clone();
    let result = storage
        .write(
            owner(),
            store::Lane::Entities,
            Box::new(move |u| {
                u.create(team("must-rollback"))?;
                assert!(
                    u.move_to_end(EntityKind::Task, "tsk_test1", Revision::ONE)
                        .is_err()
                );
                seen.store(
                    u.list(&Filter::Config, &PageRequest::default()).is_err(),
                    Ordering::SeqCst,
                );
                Ok(())
            }),
        )
        .await;
    assert!(
        result.is_err(),
        "executor must reject the swallowed cleanup failure"
    );
    assert!(
        refused.load(Ordering::SeqCst),
        "native page must refuse an aborted unit"
    );
    assert!(storage.failure_state().failure().is_none());
    storage
        .transaction(owner(), store::Lane::Entities, |u| {
            u.create(team("following"))
        })
        .await
        .unwrap();
    let snapshot = storage.read(owner()).await.unwrap();
    assert!(
        snapshot
            .get(EntityKind::Team, "must-rollback")
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        snapshot
            .get(EntityKind::Team, "following")
            .await
            .unwrap()
            .is_some()
    );
    storage.close().await.unwrap();
}

#[tokio::test]
async fn invalid_non_task_limit_precedes_malformed_cursor() {
    let (_temp, data) = crate::sqlite::test_data();
    let storage = SqliteStorage::open(Arc::new(data), StorageOptions::default()).unwrap();
    let snapshot = storage.read(owner()).await.unwrap();
    for limit in [0, 1001] {
        let result = snapshot
            .list(
                &Filter::Teams,
                &PageRequest {
                    limit,
                    cursor: Some(store::Cursor::new("malformed".into()).unwrap()),
                    ..Default::default()
                },
            )
            .await;
        assert!(
            matches!(result, Err(StoreError::InvalidPage(_))),
            "{result:?}"
        );
    }
    storage.close().await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn close_drains_admitted_write_and_releases_lock_with_live_snapshot() {
    use crate::data_dir::{DataDir, Holder, LockKind};
    let (temp, data) = crate::sqlite::test_data();
    let storage = SqliteStorage::open(Arc::new(data), StorageOptions::default()).unwrap();
    let snapshot = storage.read(owner()).await.unwrap();
    let (release, blocked) = mpsc::sync_channel(1);
    let (entered, running) = oneshot::channel();
    let writer = storage.clone();
    let writing = tokio::spawn(async move {
        writer
            .write(
                owner(),
                store::Lane::Entities,
                Box::new(move |u| {
                    u.create(team("admitted"))?;
                    let _ = entered.send(());
                    blocked.recv().unwrap();
                    Ok(())
                }),
            )
            .await
    });
    running.await.unwrap();
    let close = storage.clone();
    let closing = tokio::spawn(async move { close.close().await });
    tokio::task::yield_now().await;
    release.send(()).unwrap();
    writing.await.unwrap().unwrap();
    tokio::time::timeout(Duration::from_secs(1), closing)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(snapshot.get(EntityKind::Team, "admitted").await.is_err());
    let second = DataDir::acquire(
        &temp.path().join("data"),
        &Holder {
            kind: LockKind::Daemon,
            pid: std::process::id(),
            started_at: "synthetic".into(),
            binary_version: "test".into(),
            executable: "/synthetic/orch".into(),
        },
    )
    .unwrap();
    let conn = open_connection(&second, ConnectionRole::Reader, Synchronous::Normal).unwrap();
    let count: i64 = conn
        .query_row(
            "SELECT count(*) FROM teams WHERE project_id='p' AND id='admitted'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
}

#[tokio::test]
async fn cancelled_open_never_enters_pool_or_resets_its_failure_streak() {
    let (_temp, data) = crate::sqlite::test_data();
    let storage = SqliteStorage::open(
        Arc::new(data),
        StorageOptions {
            read_connections: 1,
            ..Default::default()
        },
    )
    .unwrap();
    let failed = worker_snapshot(&storage).await;
    assert!(
        failed
            .call::<()>(|_| Err(SqliteError::Sql(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_IOERR),
                None,
            ))))
            .await
            .is_err()
    );
    assert_eq!(storage.failure_state().pool_failure_streak(), 1);
    let (_commands, commands) = mpsc::sync_channel(16);
    let (ready, answer) = oneshot::channel();
    drop(answer);
    storage
        .owner
        .reads
        .try_send(OpenRead {
            caller: owner(),
            deadline: Instant::now() + Duration::from_secs(2),
            commands,
            ready,
        })
        .unwrap_or_else(|_| panic!("cancelled test admission"));
    let next = worker_snapshot(&storage).await;
    assert_eq!(
        storage.failure_state().pool_failure_streak(),
        1,
        "cancelled open must never count as a successful pool read"
    );
    assert!(
        next.get(EntityKind::Team, "absent")
            .await
            .unwrap()
            .is_none()
    );
    storage.close().await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn last_storage_drop_releases_lock_while_snapshot_metadata_remains() {
    use crate::data_dir::{DataDir, Holder, LockKind};
    let (temp, data) = crate::sqlite::test_data();
    let storage = SqliteStorage::open(
        Arc::new(data),
        StorageOptions {
            snapshot_idle: Duration::from_secs(60),
            ..Default::default()
        },
    )
    .unwrap();
    let snapshot = storage.read(owner()).await.unwrap();
    drop(storage);
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Ok(second) = DataDir::acquire(
                &temp.path().join("data"),
                &Holder {
                    kind: LockKind::Daemon,
                    pid: std::process::id(),
                    started_at: "synthetic".into(),
                    binary_version: "test".into(),
                    executable: "/synthetic/orch".into(),
                },
            ) {
                drop(second);
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("last storage client must stop owner and release lock");
    assert!(snapshot.get(EntityKind::Team, "absent").await.is_err());
}

#[tokio::test]
async fn first_fatal_writer_read_replies_unavailable_and_rolls_back() {
    let (_temp, data) = crate::sqlite::test_data();
    let data = Arc::new(data);
    let storage = SqliteStorage::open(data.clone(), StorageOptions::default()).unwrap();
    storage
        .transaction(owner(), store::Lane::Entities, |u| {
            u.create(team("damaged"))
        })
        .await
        .unwrap();
    open_connection(&data, ConnectionRole::Writer, Synchronous::Normal)
        .unwrap()
        .execute(
            "DELETE FROM entity_details WHERE project_id='p' AND family='team'",
            [],
        )
        .unwrap();
    let refused = Arc::new(AtomicBool::new(false));
    let seen = refused.clone();
    let result = storage
        .write(
            owner(),
            store::Lane::Entities,
            Box::new(move |u| {
                let result = u.get(EntityKind::Team, "damaged");
                seen.store(
                    matches!(&result, Err(e) if e.to_string().contains("storage_unavailable")),
                    Ordering::SeqCst,
                );
                result.map(|_| ())
            }),
        )
        .await;
    assert!(
        matches!(result, Err(ref e) if e.to_string().contains("storage_unavailable")),
        "{result:?}"
    );
    assert!(
        refused.load(Ordering::SeqCst),
        "the unit must expose the first fatal refusal before the callback handles it"
    );
    assert_eq!(
        storage.failure_state().failure().unwrap().source,
        FailureSource::Writer
    );
    storage.close().await.unwrap();
}

#[tokio::test]
async fn failed_savepoint_cleanup_with_active_transaction_refuses_native_list() {
    use orch_core::store::{ConfigEntry, UnitOfWork};
    use rusqlite::hooks::{AuthAction, AuthContext, Authorization, TransactionOperation};
    let (_temp, data) = crate::sqlite::test_data();
    let data = Arc::new(data);
    let storage = SqliteStorage::open(data.clone(), StorageOptions::default()).unwrap();
    let mut conn = open_connection(&data, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    conn.for_migration()
        .authorizer(Some(|ctx: AuthContext<'_>| match ctx.action {
            AuthAction::Savepoint {
                savepoint_name: "entity_task_move",
                operation: TransactionOperation::Release | TransactionOperation::Rollback,
            } => Authorization::Deny,
            _ => Authorization::Allow,
        }))
        .unwrap();
    let tx = conn.transaction().unwrap();
    let caller = owner();
    let mut unit = Unit::new(&tx, &caller, &storage.owner.shared);
    let mut task = orch_core::test_support::make_task();
    task.created_at = "2025-01-01T00:00:00.000Z".into();
    task.updated_at = task.created_at.clone();
    unit.create(Entity::Task(Box::new(task))).unwrap();
    unit.create(Entity::Config(ConfigEntry {
        key: "kept".into(),
        value: "v".into(),
    }))
    .unwrap();
    assert!(
        unit.move_to_end(EntityKind::Task, "tsk_test1", Revision::ONE)
            .is_err()
    );
    assert!(
        !tx.is_autocommit(),
        "exercise the active-transaction guard, not the leaf's autocommit guard"
    );
    assert!(storage.failure_state().transaction_must_abort().is_some());
    assert!(
        unit.list(&Filter::Config, &PageRequest::default()).is_err(),
        "page bridge must refuse a poisoned unit even with the transaction open"
    );
    drop(unit);
    tx.rollback().unwrap();
    drop(conn);
    storage.close().await.unwrap();
}
