//! Paging, ordering and filter scenarios shared by every v2 registration.

use super::*;

async fn assert_size_one_pages(s: &dyn Storage, filter: &Filter, sort: Sort, expected: &[&str]) {
    let mut cursor = None;
    for (index, id) in expected.iter().enumerate() {
        let page = list(
            s,
            owner("p"),
            filter,
            &PageRequest {
                limit: 1,
                cursor,
                sort,
                with_count: true,
            },
        )
        .await;
        assert_eq!(ids(&page), [*id], "page {index}, {filter:?}, {sort:?}");
        assert_eq!(page.total, Some(expected.len() as u64));
        assert_eq!(page.next_cursor.is_some(), index + 1 < expected.len());
        cursor = page.next_cursor;
    }
}

pub async fn page_bounds_defaults_and_optional_count(b: Backend) {
    let s = b.storage.as_ref();
    s.write(
        owner("p"),
        Lane::Entities,
        Box::new(move |u| {
            for n in 0..103 {
                u.create(entity(&format!("t{n:03}")))?;
            }
            Ok(())
        }),
    )
    .await
    .unwrap();
    let first = list(s, owner("p"), &tasks(), &PageRequest::default()).await;
    assert_eq!((first.items.len(), first.total), (100, None));
    assert!(first.next_cursor.as_ref().unwrap().as_str().len() <= 512);
    let next = list(
        s,
        owner("p"),
        &tasks(),
        &PageRequest {
            cursor: first.next_cursor,
            with_count: true,
            ..Default::default()
        },
    )
    .await;
    assert_eq!(ids(&next), ["t100", "t101", "t102"]);
    assert_eq!(next.total, Some(103));
    assert_eq!(next.next_cursor, None);
    for limit in [0, 1001, usize::MAX] {
        assert!(matches!(
            s.read(owner("p"))
                .await
                .unwrap()
                .list(
                    &tasks(),
                    &PageRequest {
                        limit,
                        ..Default::default()
                    }
                )
                .await,
            Err(StoreError::InvalidPage(_))
        ));
    }
    let all = list(
        s,
        owner("p"),
        &tasks(),
        &PageRequest {
            limit: 1000,
            ..Default::default()
        },
    )
    .await;
    assert_eq!(all.items.len(), 103);
    let empty = list(
        s,
        owner("empty"),
        &tasks(),
        &PageRequest {
            with_count: true,
            ..Default::default()
        },
    )
    .await;
    assert_eq!(
        (empty.items.len(), empty.total, empty.next_cursor),
        (0, Some(0), None)
    );
}

pub async fn cursor_walk_is_stable_under_inserts_and_deletes(b: Backend) {
    let s = b.storage.as_ref();
    for id in ["b", "d", "f", "h"] {
        create(s, owner("p"), entity(id)).await;
    }
    let first = list(
        s,
        owner("p"),
        &tasks(),
        &PageRequest {
            limit: 2,
            sort: Sort::Id,
            ..Default::default()
        },
    )
    .await;
    assert_eq!(ids(&first), ["b", "d"]);
    s.write(
        owner("p"),
        Lane::Entities,
        Box::new(move |u| {
            // Remove the cursor anchor, insert before and after it, and update an
            // already visited row. Two inserts before one deleted anchor shift offsets
            // by +1, so offset paging returns c instead of continuing at e.
            u.delete(EntityKind::Task, "d", Revision::ONE)?;
            for id in ["a", "c", "e", "z"] {
                u.create(entity(id))?;
            }
            u.update(
                Entity::Task(Box::new(Task {
                    title: "changed".into(),
                    ..task("b")
                })),
                Revision::ONE,
            )?;
            Ok(())
        }),
    )
    .await
    .unwrap();
    let next = list(
        s,
        owner("p"),
        &tasks(),
        &PageRequest {
            limit: 2,
            sort: Sort::Id,
            cursor: first.next_cursor,
            ..Default::default()
        },
    )
    .await;
    assert_eq!(ids(&next), ["e", "f"]);
    let last = list(
        s,
        owner("p"),
        &tasks(),
        &PageRequest {
            limit: 2,
            sort: Sort::Id,
            cursor: next.next_cursor,
            ..Default::default()
        },
    )
    .await;
    assert_eq!(ids(&last), ["h", "z"]);
    assert_eq!(last.next_cursor, None);
}

pub async fn cursor_is_bound_to_project_filter_and_sort(b: Backend) {
    let s = b.storage.as_ref();
    for id in ["a", "b"] {
        create(s, owner("p"), entity(id)).await;
    }
    let first = list(
        s,
        owner("p"),
        &tasks(),
        &PageRequest {
            limit: 1,
            ..Default::default()
        },
    )
    .await;
    let request = PageRequest {
        cursor: first.next_cursor,
        ..Default::default()
    };
    for (caller, filter, sort) in [
        (owner("other"), tasks(), Sort::Default),
        (
            owner("p"),
            Filter::Tasks {
                status: Some(TaskStatus::Todo),
                goal: None,
            },
            Sort::Default,
        ),
        (owner("p"), tasks(), Sort::Id),
        (owner("p"), Filter::Agents, Sort::Default),
    ] {
        assert!(matches!(
            s.read(caller)
                .await
                .unwrap()
                .list(
                    &filter,
                    &PageRequest {
                        sort,
                        ..request.clone()
                    }
                )
                .await,
            Err(StoreError::InvalidCursor)
        ));
    }
    for raw in [
        "not a cursor".to_string(),
        "[1]".to_string(),
        "x".repeat(513),
    ] {
        let cursor: Cursor = serde_json::from_value(serde_json::json!(raw)).unwrap();
        assert!(matches!(
            s.read(owner("p"))
                .await
                .unwrap()
                .list(
                    &tasks(),
                    &PageRequest {
                        cursor: Some(cursor),
                        ..Default::default()
                    }
                )
                .await,
            Err(StoreError::InvalidCursor)
        ));
    }
}

pub async fn list_sorts_and_filters_match_the_reference(b: Backend) {
    let s = b.storage.as_ref();
    s.write(
        owner("p"),
        Lane::Entities,
        Box::new(move |u| {
            for (id, status, time, goal) in [
                ("done", TaskStatus::Done, "z", None),
                ("old", TaskStatus::Todo, "a", Some("g")),
                ("tie", TaskStatus::Todo, "a", Some("g")),
                ("new", TaskStatus::Todo, "z", None),
                ("active", TaskStatus::InProgress, "a", None),
            ] {
                u.create(Entity::Task(Box::new(Task {
                    status,
                    updated_at: task_sort_time(time).into(),
                    goal_id: goal.map(str::to_owned),
                    ..task(id)
                })))?;
            }
            Ok(())
        }),
    )
    .await
    .unwrap();
    assert_eq!(
        ids(&list(s, owner("p"), &tasks(), &PageRequest::default()).await),
        ["active", "new", "old", "tie", "done"]
    );
    let selected = Filter::Tasks {
        status: Some(TaskStatus::Todo),
        goal: Some("g".into()),
    };
    let page = list(
        s,
        owner("p"),
        &selected,
        &PageRequest {
            with_count: true,
            ..Default::default()
        },
    )
    .await;
    assert_eq!(ids(&page), ["old", "tie"]);
    assert_eq!(page.total, Some(2));
    let page = list(
        s,
        owner("p"),
        &tasks(),
        &PageRequest {
            sort: Sort::Position,
            ..Default::default()
        },
    )
    .await;
    assert_eq!(ids(&page), ["done", "old", "tie", "new", "active"]);
    assert!(matches!(
        s.read(owner("p"))
            .await
            .unwrap()
            .list(
                &tasks(),
                &PageRequest {
                    sort: Sort::Name,
                    ..Default::default()
                }
            )
            .await,
        Err(StoreError::InvalidPage(_))
    ));
}

pub async fn declared_secondary_sorts_use_values_then_tie_breakers(b: Backend) {
    let s = b.storage.as_ref();
    for (id, priority, created, updated) in
        [("z", 2, "a", "a"), ("b", 1, "b", "c"), ("a", 1, "c", "b")]
    {
        create(
            s,
            owner("p"),
            Entity::Task(Box::new(Task {
                priority,
                created_at: task_sort_time(created).into(),
                updated_at: task_sort_time(updated).into(),
                ..task(id)
            })),
        )
        .await;
    }
    for (sort, expected) in [
        (Sort::Id, vec!["a", "b", "z"]),
        (Sort::Priority, vec!["b", "a", "z"]),
        (Sort::CreatedAt, vec!["z", "b", "a"]),
        (Sort::UpdatedAt, vec!["b", "a", "z"]),
    ] {
        assert_eq!(
            ids(&list(
                s,
                owner("p"),
                &tasks(),
                &PageRequest {
                    sort,
                    ..Default::default()
                }
            )
            .await),
            expected
        );
    }
    for (id, name) in [("z", "Alpha"), ("b", "Beta"), ("a", "Beta")] {
        create(
            s,
            owner("p"),
            Entity::Agent(Box::new(Agent {
                name: name.into(),
                ..agent(id)
            })),
        )
        .await;
        create(
            s,
            owner("p"),
            Entity::Team(Box::new(Team {
                name: name.into(),
                ..team(id)
            })),
        )
        .await;
    }
    assert_eq!(
        ids(&list(
            s,
            owner("p"),
            &Filter::Agents,
            &PageRequest {
                sort: Sort::Name,
                ..Default::default()
            }
        )
        .await),
        ["z", "b", "a"]
    );
    assert_eq!(
        ids(&list(
            s,
            owner("p"),
            &Filter::Teams,
            &PageRequest {
                sort: Sort::Name,
                ..Default::default()
            }
        )
        .await),
        ["z", "a", "b"]
    );
    assert_eq!(
        ids(&list(s, owner("p"), &Filter::Teams, &PageRequest::default()).await),
        ["a", "b", "z"]
    );
    // Insert absence before present-empty and reverse IDs within timestamp ties:
    // presence precedes position, while position precedes ID within a tie.
    for (id, updated, status) in [
        ("missing", None, GoalStatus::Active),
        ("z-empty", Some(""), GoalStatus::Active),
        ("a-empty", Some(""), GoalStatus::Active),
        ("old", Some("a"), GoalStatus::Active),
        ("z-new", Some("z"), GoalStatus::Active),
        ("a-new", Some("z"), GoalStatus::Active),
        ("paused", Some("zz"), GoalStatus::Paused),
        ("achieved", Some("zzz"), GoalStatus::Achieved),
        ("abandoned", Some("zzzz"), GoalStatus::Abandoned),
    ] {
        create(
            s,
            owner("p"),
            Entity::Goal(Box::new(Goal {
                status,
                updated_at: updated.map(str::to_owned),
                ..goal(id)
            })),
        )
        .await;
    }
    for (filter, sort, expected) in [
        (
            Filter::Goals { status: None },
            Sort::Default,
            vec![
                "z-new",
                "a-new",
                "old",
                "z-empty",
                "a-empty",
                "missing",
                "paused",
                "achieved",
                "abandoned",
            ],
        ),
        (
            Filter::Goals {
                status: Some(GoalStatus::Active),
            },
            Sort::Default,
            vec!["z-new", "a-new", "old", "z-empty", "a-empty", "missing"],
        ),
        (
            Filter::Goals {
                status: Some(GoalStatus::Paused),
            },
            Sort::Default,
            vec!["paused"],
        ),
        (
            Filter::Goals { status: None },
            Sort::CreatedAt,
            vec![
                "missing",
                "z-empty",
                "a-empty",
                "old",
                "z-new",
                "a-new",
                "paused",
                "achieved",
                "abandoned",
            ],
        ),
        (
            Filter::Goals { status: None },
            Sort::Id,
            vec![
                "a-empty",
                "a-new",
                "abandoned",
                "achieved",
                "missing",
                "old",
                "paused",
                "z-empty",
                "z-new",
            ],
        ),
    ] {
        assert_size_one_pages(s, &filter, sort, &expected).await;
    }
}

pub async fn message_and_run_filters_stay_within_their_family(b: Backend) {
    let s = b.storage.as_ref();
    // Numeric attempts disagree with timestamp, insertion and ID orders. The
    // attempt tie must use ID, even when both position and timestamp oppose it.
    for (id, task_id, agent_id, attempt, time) in [
        ("z-ten", "t1", "a1", 10, "b"),
        ("a-two", "t1", "a1", 2, "z"),
        ("b-ten", "t1", "a1", 10, "a"),
        ("other-agent", "t1", "a2", 20, "c"),
        ("other-task", "t2", "a1", 30, "d"),
    ] {
        create(
            s,
            owner("p"),
            Entity::Run(Box::new(Run {
                task_id: task_id.into(),
                agent_id: agent_id.into(),
                attempt,
                ..run(id, time)
            })),
        )
        .await;
    }
    for (task, agent, sort, expected) in [
        (
            Some("t1"),
            Some("a1"),
            Sort::Default,
            vec!["b-ten", "z-ten", "a-two"],
        ),
        (
            Some("t1"),
            None,
            Sort::Default,
            vec!["other-agent", "b-ten", "z-ten", "a-two"],
        ),
        (
            Some("t1"),
            Some("a1"),
            Sort::Id,
            vec!["a-two", "b-ten", "z-ten"],
        ),
        (
            Some("t1"),
            None,
            Sort::Id,
            vec!["a-two", "b-ten", "other-agent", "z-ten"],
        ),
        (
            None,
            Some("a1"),
            Sort::Default,
            vec!["a-two", "other-task", "z-ten", "b-ten"],
        ),
        (
            None,
            None,
            Sort::Default,
            vec!["a-two", "other-task", "other-agent", "z-ten", "b-ten"],
        ),
        (
            None,
            None,
            Sort::Id,
            vec!["a-two", "b-ten", "other-agent", "other-task", "z-ten"],
        ),
    ] {
        let filter = Filter::Runs {
            task: task.map(str::to_owned),
            agent: agent.map(str::to_owned),
        };
        assert_size_one_pages(s, &filter, sort, &expected).await;
    }
    for (id, recipient, time) in [
        ("z", Some("a"), "b"),
        ("a", None, "a"),
        ("b", Some("b"), "c"),
    ] {
        create(
            s,
            owner("p"),
            Entity::Message(Box::new(Message {
                to_agent_id: recipient.map(str::to_owned),
                created_at: time.into(),
                ..message(id)
            })),
        )
        .await;
    }
    assert_eq!(
        ids(&list(
            s,
            owner("p"),
            &Filter::Messages { recipient: None },
            &PageRequest::default()
        )
        .await),
        ["a", "z", "b"]
    );
    assert_eq!(
        ids(&list(
            s,
            owner("p"),
            &Filter::Messages {
                recipient: Some("b".into())
            },
            &PageRequest::default()
        )
        .await),
        ["b"]
    );
}

// Task's persisted timestamp contract is canonical millisecond UTC. Keep the
// original lexical ordering vectors while using representable Task values.
fn task_sort_time(value: &str) -> &'static str {
    match value {
        "a" => "2025-01-01T00:00:00.000Z",
        "b" => "2025-01-02T00:00:00.000Z",
        "c" => "2025-01-03T00:00:00.000Z",
        "z" => "2025-01-26T00:00:00.000Z",
        _ => unreachable!("fixed task ordering vector"),
    }
}
