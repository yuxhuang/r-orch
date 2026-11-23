//! Consumer-facing selection contracts. The expectations are independent of the
//! backend's filter and key implementations.

use super::*;

fn pending(recipient: &str, now: &str) -> Filter {
    Filter::PendingMessages {
        recipient: recipient.into(),
        now: now.into(),
    }
}

pub async fn pending_messages_filter_status_recipient_and_lexical_expiry(b: Backend) {
    let s = b.storage.as_ref();
    for (id, recipient, status, expiry) in [
        ("absent", Some("agt_b"), MessageStatus::Pending, None),
        ("equal", Some("agt_b"), MessageStatus::Pending, Some(STAMP)),
        (
            "future",
            Some("agt_b"),
            MessageStatus::Pending,
            Some("2030"),
        ),
        (
            "invalid-high",
            Some("agt_b"),
            MessageStatus::Pending,
            Some("z-not-a-date"),
        ),
        (
            "invalid-low",
            Some("agt_b"),
            MessageStatus::Pending,
            Some("!not-a-date"),
        ),
        ("past", Some("agt_b"), MessageStatus::Pending, Some("2000")),
        ("delivered", Some("agt_b"), MessageStatus::Delivered, None),
        (
            "expired-status",
            Some("agt_b"),
            MessageStatus::Expired,
            None,
        ),
        ("null-broadcast", None, MessageStatus::Pending, None),
        ("case", Some("AGT_B"), MessageStatus::Pending, None),
        ("prefix", Some("agt_b_suffix"), MessageStatus::Pending, None),
        ("other", Some("agt_c"), MessageStatus::Pending, None),
    ] {
        let mut m = message(id);
        m.to_agent_id = recipient.map(str::to_owned);
        m.status = status;
        m.expires_at = expiry.map(str::to_owned);
        if recipient.is_none() {
            m.channel = MessageChannel::Broadcast;
        }
        create(s, owner("p"), Entity::Message(Box::new(m))).await;
    }
    create(
        s,
        owner("q"),
        Entity::Message(Box::new(message("other-project"))),
    )
    .await;
    let page = list(
        s,
        owner("p"),
        &pending("agt_b", STAMP),
        &PageRequest {
            with_count: true,
            ..Default::default()
        },
    )
    .await;
    assert_eq!(ids(&page), ["absent", "equal", "future", "invalid-high"]);
    assert_eq!(page.total, Some(4));
    assert_eq!(page.next_cursor, None);
    let empty = list(s, owner("p"), &pending("", STAMP), &PageRequest::default()).await;
    assert!(empty.items.is_empty());
    // No clock/date validation: comparison of even an empty now is lexical.
    let early = list(
        s,
        owner("p"),
        &pending("agt_b", ""),
        &PageRequest::default(),
    )
    .await;
    assert_eq!(
        ids(&early),
        [
            "absent",
            "equal",
            "future",
            "invalid-high",
            "invalid-low",
            "past"
        ]
    );
}

pub async fn pending_messages_page_matches_before_count_cursor_and_bound(b: Backend) {
    let s = b.storage.as_ref();
    s.write(
        owner("p"),
        Lane::Entities,
        Box::new(|u| {
            for n in 0..1100 {
                let mut m = message(&format!("excluded-{n:04}"));
                m.status = MessageStatus::Delivered;
                u.create(Entity::Message(Box::new(m)))?;
            }
            for id in ["a", "b", "c"] {
                u.create(Entity::Message(Box::new(message(id))))?;
            }
            // A unit of work sees matching rows it just created too.
            let page = u.list(
                &pending("agt_b", STAMP),
                &PageRequest {
                    limit: 2,
                    with_count: true,
                    ..Default::default()
                },
            )?;
            assert_eq!(ids(&page), ["a", "b"]);
            assert_eq!(page.total, Some(3));
            Ok(())
        }),
    )
    .await
    .unwrap();
    let snapshot = s.read(owner("p")).await.unwrap();
    let filter = pending("agt_b", STAMP);
    let first = snapshot
        .list(
            &filter,
            &PageRequest {
                limit: 2,
                with_count: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(ids(&first), ["a", "b"]);
    assert_eq!(first.total, Some(3));
    let cursor = first.next_cursor.unwrap();
    // Commit an insert and a status change between pages on the same snapshot.
    s.write(
        owner("p"),
        Lane::Entities,
        Box::new(|u| {
            u.create(Entity::Message(Box::new(message("later"))))?;
            let mut m = message("c");
            m.status = MessageStatus::Delivered;
            u.update(Entity::Message(Box::new(m)), Revision::ONE)?;
            Ok(())
        }),
    )
    .await
    .unwrap();
    let next = PageRequest {
        limit: 2,
        cursor: Some(cursor.clone()),
        with_count: true,
        ..Default::default()
    };
    let last = snapshot.list(&filter, &next).await.unwrap();
    assert_eq!(ids(&last), ["c"]);
    assert_eq!(last.total, Some(3));
    assert_eq!(last.next_cursor, None);
    let live = list(s, owner("p"), &filter, &PageRequest::default()).await;
    assert_eq!(ids(&live), ["a", "b", "later"]);
    for changed in [
        pending("agt_c", STAMP),
        pending("agt_b", "2030"),
        Filter::Messages {
            recipient: Some("agt_b".into()),
        },
    ] {
        assert!(matches!(
            snapshot.list(&changed, &next).await,
            Err(StoreError::InvalidCursor)
        ));
    }
    assert!(matches!(
        snapshot
            .list(
                &filter,
                &PageRequest {
                    sort: Sort::Id,
                    ..next.clone()
                }
            )
            .await,
        Err(StoreError::InvalidCursor)
    ));
    assert!(matches!(
        s.read(owner("q")).await.unwrap().list(&filter, &next).await,
        Err(StoreError::InvalidCursor)
    ));
}

fn task_runs(task: Option<&str>, agent: Option<&str>) -> Filter {
    Filter::Runs {
        task: task.map(str::to_owned),
        agent: agent.map(str::to_owned),
    }
}

fn attempted(id: &str, attempt: u32, task: &str, agent: &str, started: &str) -> Entity {
    let mut r = run(id, started);
    r.attempt = attempt;
    r.task_id = task.into();
    r.agent_id = agent.into();
    Entity::Run(Box::new(r))
}

pub async fn run_attempt_order_is_numeric_task_scoped_and_keeps_default(b: Backend) {
    let s = b.storage.as_ref();
    for (id, attempt, task, agent, started) in [
        ("one", 1, "t", "a", "2030"),
        ("two", 2, "t", "a", "2029"),
        ("ten-z", 10, "t", "a", "2020"),
        ("ten-a", 10, "t", "a", "2020"),
        ("max", u32::MAX, "t", "a", "2010"),
        ("other-task", u32::MAX, "other", "a", "2040"),
        ("other-agent", 20, "t", "b", "2000"),
    ] {
        create(s, owner("p"), attempted(id, attempt, task, agent, started)).await;
    }
    create(
        s,
        owner("q"),
        attempted("other-project", u32::MAX, "t", "a", STAMP),
    )
    .await;
    let filter = task_runs(Some("t"), None);
    let page = list(
        s,
        owner("p"),
        &filter,
        &PageRequest {
            sort: Sort::Attempt,
            with_count: true,
            ..Default::default()
        },
    )
    .await;
    assert_eq!(
        ids(&page),
        ["max", "other-agent", "ten-a", "ten-z", "two", "one"]
    );
    assert_eq!(page.total, Some(6));
    let filtered = list(
        s,
        owner("p"),
        &task_runs(Some("t"), Some("a")),
        &PageRequest {
            sort: Sort::Attempt,
            ..Default::default()
        },
    )
    .await;
    assert_eq!(ids(&filtered), ["max", "ten-a", "ten-z", "two", "one"]);
    let default = list(s, owner("p"), &filter, &PageRequest::default()).await;
    assert_eq!(
        ids(&default),
        ["one", "two", "ten-a", "ten-z", "max", "other-agent"]
    );
    let all = list(
        s,
        owner("p"),
        &task_runs(None, None),
        &PageRequest::default(),
    )
    .await;
    assert_eq!(ids(&all)[0], "other-task");
}

pub async fn run_attempt_pages_keep_snapshot_and_bind_every_query_field(b: Backend) {
    let s = b.storage.as_ref();
    for (id, n) in [("a", u32::MAX), ("b", 10), ("c", 10), ("d", 2), ("e", 1)] {
        create(s, owner("p"), attempted(id, n, "t", "a", STAMP)).await;
    }
    let filter = task_runs(Some("t"), Some("a"));
    let snapshot = s.read(owner("p")).await.unwrap();
    let page = PageRequest {
        limit: 2,
        sort: Sort::Attempt,
        with_count: true,
        ..Default::default()
    };
    let first = snapshot.list(&filter, &page).await.unwrap();
    assert_eq!(ids(&first), ["a", "b"]);
    assert_eq!(first.total, Some(5));
    let next = PageRequest {
        cursor: first.next_cursor.clone(),
        ..page.clone()
    };
    s.write(
        owner("p"),
        Lane::Entities,
        Box::new(|u| {
            u.delete(EntityKind::Run, "c", Revision::ONE)?;
            u.create(attempted("later", 3, "t", "a", STAMP))?;
            let result = u.list(
                &task_runs(Some("t"), Some("a")),
                &PageRequest {
                    sort: Sort::Attempt,
                    ..Default::default()
                },
            )?;
            assert_eq!(ids(&result), ["a", "b", "later", "d", "e"]);
            Ok(())
        }),
    )
    .await
    .unwrap();
    let second = snapshot.list(&filter, &next).await.unwrap();
    assert_eq!(ids(&second), ["c", "d"]);
    assert_eq!(second.total, Some(5));
    let last = snapshot
        .list(
            &filter,
            &PageRequest {
                cursor: second.next_cursor,
                ..page.clone()
            },
        )
        .await
        .unwrap();
    assert_eq!(ids(&last), ["e"]);
    assert_eq!(last.next_cursor, None);
    for changed in [
        task_runs(Some("other"), Some("a")),
        task_runs(Some("t"), Some("b")),
        task_runs(Some("t"), None),
    ] {
        assert!(matches!(
            snapshot.list(&changed, &next).await,
            Err(StoreError::InvalidCursor)
        ));
    }
    for sort in [Sort::Default, Sort::Id] {
        assert!(matches!(
            snapshot
                .list(
                    &filter,
                    &PageRequest {
                        sort,
                        ..next.clone()
                    }
                )
                .await,
            Err(StoreError::InvalidCursor)
        ));
    }
    assert!(matches!(
        s.read(owner("q")).await.unwrap().list(&filter, &next).await,
        Err(StoreError::InvalidCursor)
    ));
    // A cursor holds a key, not a row or a snapshot: deleting its row is safe.
    s.write(
        owner("p"),
        Lane::Entities,
        Box::new(|u| u.delete(EntityKind::Run, "b", Revision::ONE)),
    )
    .await
    .unwrap();
    let live = list(s, owner("p"), &filter, &next).await;
    assert_eq!(ids(&live), ["later", "d"]);
}

pub async fn run_attempt_sort_refuses_every_unscoped_or_other_family_filter(b: Backend) {
    let snapshot = b.storage.read(owner("p")).await.unwrap();
    let invalid = [
        task_runs(None, None),
        task_runs(None, Some("a")),
        tasks(),
        Filter::Goals { status: None },
        Filter::Agents,
        Filter::Teams,
        Filter::Messages { recipient: None },
        pending("agt_b", STAMP),
        Filter::Context,
        Filter::Config,
        Filter::Templates,
    ];
    for filter in invalid {
        for cursor in [None, Some(Cursor::new("unknown".into()).unwrap())] {
            assert!(matches!(
                snapshot
                    .list(
                        &filter,
                        &PageRequest {
                            sort: Sort::Attempt,
                            cursor,
                            ..Default::default()
                        }
                    )
                    .await,
                Err(StoreError::InvalidPage(_))
            ));
        }
    }
}
