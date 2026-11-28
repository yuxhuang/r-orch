//! Step 5 non-Task cursor proofs through the owned public Storage adapter.
//! Synthetic entities drive native encoding, retained/lookup state, same-view
//! hydration and leaf selection together (issue 370). No hand-built cursor table.
use orch_core::store::{
    Caller, ConfigEntry, Cursor, Entity, EntityKind, Filter, Lane, Page, PageRequest, Principal,
    Record, Revision, Sort, Storage, StorageExt, StoreError, TemplateEntry,
};
use orch_core::test_support::{make_agent, make_run};
use orch_domain::{Goal, GoalStatus};
use orch_infra::{
    data_dir::{DataDir, Holder, LockKind},
    sqlite::{SqliteStorage, StorageOptions},
};
use serde_json::json;
use std::sync::Arc;

fn owner(project: &str) -> Caller {
    Caller {
        project: project.into(),
        principal: Principal::Owner {
            identity: "synthetic".into(),
        },
    }
}

fn storage() -> (tempfile::TempDir, SqliteStorage) {
    let root = tempfile::tempdir().unwrap();
    let s = open(root.path(), StorageOptions::default());
    (root, s)
}

fn open(root: &std::path::Path, options: StorageOptions) -> SqliteStorage {
    let data = DataDir::acquire(
        &root.join("data"),
        &Holder {
            kind: LockKind::Daemon,
            pid: std::process::id(),
            started_at: "synthetic".into(),
            binary_version: "test".into(),
            executable: "/synthetic/orch".into(),
        },
    )
    .unwrap();
    SqliteStorage::open(Arc::new(data), options).unwrap()
}

async fn create(s: &SqliteStorage, rows: Vec<Entity>) {
    s.transaction(owner("p"), Lane::Entities, move |u| {
        for row in rows {
            u.create(row)?;
        }
        Ok(())
    })
    .await
    .unwrap();
}

async fn page(
    s: &SqliteStorage,
    f: &Filter,
    sort: Sort,
    cursor: Option<Cursor>,
    limit: usize,
) -> Page<Record> {
    s.read(owner("p"))
        .await
        .unwrap()
        .list(
            f,
            &PageRequest {
                sort,
                cursor,
                limit,
                with_count: true,
            },
        )
        .await
        .unwrap()
}

fn ids(page: &Page<Record>) -> Vec<&str> {
    page.items.iter().map(|r| r.entity.id()).collect()
}

fn token(page: &Page<Record>, version: &str) -> Cursor {
    let token = page
        .next_cursor
        .clone()
        .expect("fixture must have a continuation");
    // Pin the exercised arm: a passing inline fixture cannot stand in for lookup.
    assert!(
        token.as_str().starts_with(version),
        "expected arm {version}"
    );
    token
}

fn goal(id: &str, created: &str, updated: Option<&str>) -> Entity {
    Entity::Goal(Box::new(Goal {
        id: id.into(),
        title: "synthetic".into(),
        description: String::new(),
        status: GoalStatus::Active,
        assignee: None,
        created_at: created.into(),
        updated_at: updated.map(str::to_owned),
    }))
}

fn message(id: &str, created: &str) -> Entity {
    Entity::Message(Box::new(
        serde_json::from_value(json!({
            "id": id, "channel": "direct", "from_agent_id": "sender", "to_agent_id": "recipient",
            "subject": "s", "body": "b", "created_at": created, "status": "pending"
        }))
        .unwrap(),
    ))
}

async fn achieve(s: &SqliteStorage, id: String) {
    s.transaction(owner("p"), Lane::Entities, move |u| {
        let row = u.get(EntityKind::Goal, &id)?.unwrap();
        let Entity::Goal(mut g) = row.entity else {
            unreachable!()
        };
        g.status = GoalStatus::Achieved;
        u.update(Entity::Goal(g), row.revision).map(drop)
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn p_f1_inline_and_retained_goals_resume_after_the_anchor_leaves_the_filter() {
    for (anchor, arm) in [("g".into(), "01"), ("g".repeat(300), "02")] {
        let (_root, s) = storage();
        let f = Filter::Goals {
            status: Some(GoalStatus::Active),
        };
        create(
            &s,
            vec![goal(&anchor, "a", Some("z")), goal("next", "b", Some("a"))],
        )
        .await;
        let first = page(&s, &f, Sort::Default, None, 1).await;
        assert_eq!(ids(&first), [anchor.as_str()]);
        let cursor = token(&first, arm);
        achieve(&s, anchor).await;
        let next = page(&s, &f, Sort::Default, Some(cursor), 1).await;
        assert_eq!(ids(&next), ["next"]);
        assert_eq!(next.total, Some(1));
        assert!(next.next_cursor.is_none());
        s.close().await.unwrap();
    }
}

#[tokio::test]
async fn p_f2_retained_run_resumes_after_deleted_300_byte_anchor() {
    let (_root, s) = storage();
    let anchor = "r".repeat(300);
    let f = Filter::Runs {
        task: None,
        agent: Some("selected".into()),
    };
    let mut rows = Vec::new();
    for (id, started, agent) in [
        (anchor.as_str(), "z", "selected"),
        ("next", "y", "selected"),
        ("excluded", "c", "other"),
    ] {
        let mut r = make_run();
        r.id = id.into();
        r.started_at = started.into();
        r.agent_id = agent.into();
        rows.push(Entity::Run(Box::new(r)));
    }
    create(&s, rows).await;
    let first = page(&s, &f, Sort::Default, None, 1).await;
    assert_eq!(ids(&first), [anchor.as_str()]);
    let cursor = token(&first, "02");
    s.transaction(owner("p"), Lane::Entities, move |u| {
        u.delete(EntityKind::Run, &anchor, Revision::ONE)
    })
    .await
    .unwrap();
    let next = page(&s, &f, Sort::Default, Some(cursor), 10).await;
    assert_eq!(ids(&next), ["next"]);
    assert_eq!(next.total, Some(1));
    assert!(next.next_cursor.is_none());
    s.close().await.unwrap();
}

#[tokio::test]
async fn q8_lookup_pending_message_resumes_after_delivery() {
    let (_root, s) = storage();
    // Both native texts fit individually; their tuple exceeds 65,536 bytes.
    let anchor = "m".repeat(10_000);
    let f = Filter::PendingMessages {
        recipient: "recipient".into(),
        now: "z".into(),
    };
    create(
        &s,
        vec![
            message(&anchor, &"a".repeat(60_000)),
            message("next", "b"),
            message("last", "c"),
        ],
    )
    .await;
    let first = page(&s, &f, Sort::Default, None, 1).await;
    assert_eq!(ids(&first), [anchor.as_str()]);
    let cursor = token(&first, "03");
    s.transaction(owner("p"), Lane::Entities, move |u| {
        let row = u.get(EntityKind::Message, &anchor)?.unwrap();
        let Entity::Message(mut m) = row.entity else {
            unreachable!()
        };
        m.status = orch_domain::MessageStatus::Delivered;
        u.update(Entity::Message(m), row.revision).map(drop)
    })
    .await
    .unwrap();
    let next = page(&s, &f, Sort::Default, Some(cursor), 10).await;
    assert_eq!(ids(&next), ["next", "last"]);
    assert_eq!(next.total, Some(2));
    assert!(next.next_cursor.is_none());
    s.close().await.unwrap();
}

// N3a and N3b use the same native-text goal; status participates in Default
// but not CreatedAt. Removing lookup tuple validation must break N3a.
async fn lookup_goal_after_achievement(sort: Sort) -> orch_core::store::Result<Page<Record>> {
    let (_root, s) = storage();
    let anchor = "g".repeat(10_000);
    let f = Filter::Goals {
        status: Some(GoalStatus::Active),
    };
    create(
        &s,
        vec![
            goal(&anchor, &"a".repeat(60_000), Some(&"z".repeat(60_000))),
            goal("next", "b", Some("a")),
        ],
    )
    .await;
    let first = page(&s, &f, sort, None, 1).await;
    assert_eq!(ids(&first), [anchor.as_str()]);
    let cursor = token(&first, "03");
    achieve(&s, anchor).await;
    let result = s
        .read(owner("p"))
        .await
        .unwrap()
        .list(
            &f,
            &PageRequest {
                sort,
                cursor: Some(cursor),
                with_count: true,
                ..Default::default()
            },
        )
        .await;
    s.close().await.unwrap();
    result
}

#[tokio::test]
async fn n3a_lookup_goal_default_refuses_changed_status_rank() {
    assert!(matches!(
        lookup_goal_after_achievement(Sort::Default).await,
        Err(StoreError::InvalidCursor)
    ));
}

#[tokio::test]
async fn n3b_lookup_goal_created_at_resumes_after_status_change() {
    let next = lookup_goal_after_achievement(Sort::CreatedAt)
        .await
        .unwrap();
    assert_eq!(ids(&next), ["next"]);
    assert_eq!(next.total, Some(1));
    assert!(next.next_cursor.is_none());
}

#[tokio::test]
async fn p_r4_lookup_goal_refuses_rewritten_updated_at() {
    let (_root, s) = storage();
    let anchor = "g".repeat(70_000);
    let f = Filter::Goals { status: None };
    create(
        &s,
        vec![goal(&anchor, "a", Some("z")), goal("next", "b", Some("a"))],
    )
    .await;
    let first = page(&s, &f, Sort::Default, None, 1).await;
    assert_eq!(ids(&first), [anchor.as_str()]);
    let cursor = token(&first, "03");
    s.transaction(owner("p"), Lane::Entities, move |u| {
        let row = u.get(EntityKind::Goal, &anchor)?.unwrap();
        let Entity::Goal(mut g) = row.entity else {
            unreachable!()
        };
        g.updated_at = Some("y".into());
        u.update(Entity::Goal(g), row.revision).map(drop)
    })
    .await
    .unwrap();
    assert!(matches!(
        s.read(owner("p"))
            .await
            .unwrap()
            .list(
                &f,
                &PageRequest {
                    cursor: Some(cursor),
                    ..Default::default()
                }
            )
            .await,
        Err(StoreError::InvalidCursor)
    ));
    s.close().await.unwrap();
}

#[tokio::test]
async fn d2_l_lookup_agent_matches_utf8_byte_length_after_unrelated_edit() {
    let (_root, s) = storage();
    let anchor = "é".repeat(30_000);
    let mut a = make_agent();
    a.id = anchor.clone();
    a.name = "a".repeat(10_000);
    let mut b = make_agent();
    b.id = "next".into();
    b.name = "b".into();
    create(
        &s,
        vec![Entity::Agent(Box::new(a)), Entity::Agent(Box::new(b))],
    )
    .await;
    let first = page(&s, &Filter::Agents, Sort::Name, None, 1).await;
    assert_eq!(ids(&first), [anchor.as_str()]);
    let cursor = token(&first, "03");
    s.transaction(owner("p"), Lane::Entities, move |u| {
        let row = u.get(EntityKind::Agent, &anchor)?.unwrap();
        let Entity::Agent(mut a) = row.entity else {
            unreachable!()
        };
        a.stats.tasks_completed += 1;
        u.update(Entity::Agent(a), row.revision).map(drop)
    })
    .await
    .unwrap();
    let next = page(&s, &Filter::Agents, Sort::Name, Some(cursor), 10).await;
    assert_eq!(ids(&next), ["next"]);
    assert!(next.next_cursor.is_none());
    s.close().await.unwrap();
}

fn families() -> Vec<Filter> {
    vec![
        Filter::Agents,
        Filter::Teams,
        Filter::Runs {
            task: None,
            agent: None,
        },
        Filter::Messages { recipient: None },
        Filter::Goals { status: None },
        Filter::Context,
        Filter::Config,
        Filter::Templates,
    ]
}

fn entity(kind: EntityKind, id: &str) -> Entity {
    match kind {
        EntityKind::Agent => { let mut a = make_agent(); a.id = id.into(); Entity::Agent(Box::new(a)) }
        EntityKind::Team => Entity::Team(Box::new(serde_json::from_value(json!({"id": id, "name": id,
            "status":"active", "members":[], "task_pool":[], "lead_agent_id":"a", "created_at":"opaque",
            "updated_at":"opaque", "config":{"auto_claim":false}})).unwrap())),
        EntityKind::Run => { let mut r = make_run(); r.id = id.into(); Entity::Run(Box::new(r)) }
        EntityKind::Message => message(id, "opaque"),
        EntityKind::Goal => goal(id, "opaque", None),
        EntityKind::Context => Entity::Context(orch_core::storage::ContextEntry {
            key: id.into(), value: "v".into(), created_at:"opaque".into(), updated_at:"opaque".into(), ttl_ms:None, expires_at:None,
        }),
        EntityKind::Config => Entity::Config(ConfigEntry { key: id.into(), value: "v".into() }),
        EntityKind::Template => Entity::Template(TemplateEntry { name: id.into(), body: "v".into() }),
        EntityKind::Task => unreachable!("Task has its own shared cursor cases"),
    }
}

#[tokio::test]
async fn cursor_walk_is_stable_under_inserts_and_deletes_for_all_eight_non_task_families() {
    // Two inserts before the deleted anchor leave a net +1 positional shift.
    // Same mutation sequence as the shared Task case, using every non-Task
    // family, in both inline and retained arms. Offset/anchor lookup is wrong.
    for f in families() {
        for (suffix, arm) in [("".into(), "01"), ("x".repeat(300), "02")] {
            let (_root, s) = storage();
            let key = |prefix: &str| format!("{prefix}{suffix}");
            create(
                &s,
                ["b", "d", "f", "h"]
                    .map(|id| entity(f.kind(), &key(id)))
                    .to_vec(),
            )
            .await;
            let first = page(&s, &f, Sort::Id, None, 2).await;
            assert_eq!(ids(&first), [key("b"), key("d")]);
            let cursor = token(&first, arm);
            let kind = f.kind();
            let suffix_copy = suffix.clone();
            s.transaction(owner("p"), Lane::Entities, move |u| {
                u.delete(kind, &format!("d{suffix_copy}"), Revision::ONE)?;
                for id in ["a", "c", "e", "z"] {
                    u.create(entity(kind, &format!("{id}{suffix_copy}")))?;
                }
                // Update a visited row without changing its ordering key.
                let row = u.get(kind, &format!("b{suffix_copy}"))?.unwrap();
                u.update(row.entity, row.revision)?;
                Ok(())
            })
            .await
            .unwrap();
            let next = page(&s, &f, Sort::Id, Some(cursor), 2).await;
            assert_eq!(ids(&next), [key("e"), key("f")], "{f:?} {arm}");
            assert_eq!(next.total, Some(7));
            let last = page(&s, &f, Sort::Id, next.next_cursor, 2).await;
            assert_eq!(ids(&last), [key("h"), key("z")], "{f:?} {arm}");
            assert!(last.next_cursor.is_none());
            s.close().await.unwrap();
        }
    }
}

#[tokio::test]
async fn cursor_is_bound_to_project_filter_and_sort_for_all_eight_non_task_families() {
    for f in families() {
        for (suffix, arm) in [
            ("".into(), "01"),
            ("x".repeat(300), "02"),
            ("x".repeat(70_000), "03"),
        ] {
            let (_root, s) = storage();
            create(
                &s,
                vec![
                    entity(f.kind(), &format!("a{suffix}")),
                    entity(f.kind(), &format!("b{suffix}")),
                ],
            )
            .await;
            let first = page(&s, &f, Sort::Id, None, 1).await;
            let cursor = token(&first, arm);
            let changed_filter = match f {
                Filter::Runs { .. } => Filter::Runs {
                    task: None,
                    agent: Some("other".into()),
                },
                Filter::Messages { .. } => Filter::Messages {
                    recipient: Some("other".into()),
                },
                Filter::Goals { .. } => Filter::Goals {
                    status: Some(GoalStatus::Active),
                },
                Filter::Agents => Filter::Teams,
                _ => Filter::Agents,
            };
            for (project, filter, sort) in [
                ("other", &f, Sort::Id),
                ("p", &changed_filter, Sort::Id),
                ("p", &f, Sort::Default),
            ] {
                let result = s
                    .read(owner(project))
                    .await
                    .unwrap()
                    .list(
                        filter,
                        &PageRequest {
                            cursor: Some(cursor.clone()),
                            sort,
                            ..Default::default()
                        },
                    )
                    .await;
                assert!(
                    matches!(result, Err(StoreError::InvalidCursor)),
                    "{f:?} {arm} {project} {sort:?}: {result:?}"
                );
            }
            // Failed bindings must not consume the live entry. Limit/count
            // changes are permitted, matching the shared case and P-X3.
            let next = page(&s, &f, Sort::Id, Some(cursor), 10).await;
            assert_eq!(ids(&next), [format!("b{suffix}")]);
            assert_eq!(next.total, Some(2));
            assert!(next.next_cursor.is_none());
            s.close().await.unwrap();
        }
    }
}

fn request(sort: Sort, cursor: Option<Cursor>, limit: usize) -> PageRequest {
    PageRequest {
        sort,
        cursor,
        limit,
        with_count: true,
    }
}

#[tokio::test]
async fn p_x4_writer_and_reader_cursors_resume_in_the_other_view_for_all_three_arms() {
    for (suffix, arm) in [
        (String::new(), "01"),
        ("x".repeat(300), "02"),
        ("x".repeat(70_000), "03"),
    ] {
        let (_root, s) = storage();
        let anchor = format!("a{suffix}");
        create(
            &s,
            vec![
                entity(EntityKind::Team, &anchor),
                entity(EntityKind::Team, "z"),
            ],
        )
        .await;
        let writer = s
            .transaction(owner("p"), Lane::Entities, |u| {
                u.list(&Filter::Teams, &request(Sort::Id, None, 1))
            })
            .await
            .unwrap();
        let writer_token = token(&writer, arm);
        let reader = s.read(owner("p")).await.unwrap();
        let next = reader
            .list(&Filter::Teams, &request(Sort::Id, Some(writer_token), 10))
            .await
            .unwrap();
        assert_eq!(ids(&next), ["z"], "writer -> reader {arm}");
        assert!(next.next_cursor.is_none());
        let first = reader
            .list(&Filter::Teams, &request(Sort::Id, None, 1))
            .await
            .unwrap();
        let reader_token = token(&first, arm);
        // The writer deletes the anchor in its view. A lookup must observe that
        // deletion, even while the issuing reader still sees the original row.
        let result = s
            .transaction(owner("p"), Lane::Entities, move |u| {
                let unchanged = u.list(
                    &Filter::Teams,
                    &request(Sort::Id, Some(reader_token.clone()), 10),
                )?;
                assert_eq!(ids(&unchanged), ["z"], "reader -> writer unchanged {arm}");
                u.delete(EntityKind::Team, &anchor, Revision::ONE)?;
                Ok(u.list(&Filter::Teams, &request(Sort::Id, Some(reader_token), 10)))
            })
            .await
            .unwrap();
        if arm == "03" {
            assert!(
                matches!(result, Err(StoreError::InvalidCursor)),
                "lookup uses writer view"
            );
        } else {
            let next = result.unwrap();
            assert_eq!(ids(&next), ["z"], "reader -> writer {arm}");
            assert_eq!(next.total, Some(1));
            assert!(next.next_cursor.is_none());
        }
        // Issuing view remains valid after the write, including lookup.
        let next = reader
            .list(&Filter::Teams, &request(Sort::Id, first.next_cursor, 10))
            .await
            .unwrap();
        assert_eq!(ids(&next), ["z"], "reader retains snapshot {arm}");
        drop(reader);
        s.close().await.unwrap();
    }
}

#[tokio::test]
async fn p_x4_lookup_from_writer_is_judged_against_an_older_reader_snapshot() {
    let (_root, s) = storage();
    create(&s, vec![entity(EntityKind::Team, "z")]).await;
    let reader = s.read(owner("p")).await.unwrap();
    // Materialize a view before the writer creates the lookup anchor.
    assert_eq!(
        ids(&reader
            .list(&Filter::Teams, &request(Sort::Name, None, 10))
            .await
            .unwrap()),
        ["z"]
    );
    let mut anchor = entity(EntityKind::Team, "anchor");
    if let Entity::Team(t) = &mut anchor {
        t.name = "a".repeat(130_000);
    }
    let first = s
        .transaction(owner("p"), Lane::Entities, move |u| {
            u.create(anchor)?;
            u.list(&Filter::Teams, &request(Sort::Name, None, 1))
        })
        .await
        .unwrap();
    let cursor = token(&first, "03");
    assert!(
        matches!(
            reader
                .list(
                    &Filter::Teams,
                    &request(Sort::Name, Some(cursor.clone()), 10)
                )
                .await,
            Err(StoreError::InvalidCursor)
        ),
        "lookup must not escape the older resuming snapshot"
    );
    let next = page(&s, &Filter::Teams, Sort::Name, Some(cursor), 10).await;
    assert_eq!(ids(&next), ["z"]);
    drop(reader);
    s.close().await.unwrap();
}

#[tokio::test]
async fn c8_prime_rolled_back_inline_and_retained_pages_resume_over_committed_rows() {
    for (suffix, arm) in [(String::new(), "01"), ("x".repeat(300), "02")] {
        let (_root, s) = storage();
        create(
            &s,
            vec![
                entity(EntityKind::Team, "b"),
                entity(EntityKind::Team, "f"),
                entity(EntityKind::Team, "h"),
            ],
        )
        .await;
        let (send, recv) = std::sync::mpsc::channel();
        let result: orch_core::store::Result<()> = s
            .transaction(owner("p"), Lane::Entities, move |u| {
                u.create(entity(EntityKind::Team, &format!("d{suffix}")))?;
                let first = u.list(&Filter::Teams, &request(Sort::Id, None, 2))?;
                send.send(token(&first, arm)).unwrap();
                Err(StoreError::InvalidCursor) // Deliberately roll the unit back.
            })
            .await;
        assert!(matches!(result, Err(StoreError::InvalidCursor)));
        let next = page(&s, &Filter::Teams, Sort::Id, Some(recv.recv().unwrap()), 10).await;
        assert_eq!(ids(&next), ["f", "h"], "rollback original boundary {arm}");
        assert_eq!(next.total, Some(3));
        assert!(next.next_cursor.is_none());
        s.close().await.unwrap();
    }
}

#[tokio::test]
async fn q2_rolled_back_lookup_team_is_invalid_in_a_later_view() {
    let (_root, s) = storage();
    create(&s, vec![entity(EntityKind::Team, "z")]).await;
    let (send, recv) = std::sync::mpsc::channel();
    let result: orch_core::store::Result<()> = s
        .transaction(owner("p"), Lane::Entities, move |u| {
            let mut anchor = entity(EntityKind::Team, "anchor");
            if let Entity::Team(t) = &mut anchor {
                t.name = "a".repeat(130_000);
            }
            u.create(anchor)?;
            let first = u.list(&Filter::Teams, &request(Sort::Name, None, 1))?;
            assert_eq!(ids(&first), ["anchor"]);
            send.send(token(&first, "03")).unwrap();
            Err(StoreError::InvalidCursor)
        })
        .await;
    assert!(matches!(result, Err(StoreError::InvalidCursor)));
    let result = s
        .read(owner("p"))
        .await
        .unwrap()
        .list(
            &Filter::Teams,
            &request(Sort::Name, Some(recv.recv().unwrap()), 10),
        )
        .await;
    assert!(
        matches!(result, Err(StoreError::InvalidCursor)),
        "rolled-back lookup must return no page"
    );
    assert_eq!(
        ids(&page(&s, &Filter::Teams, Sort::Name, None, 10).await),
        ["z"]
    );
    s.close().await.unwrap();
}

#[tokio::test]
async fn p_w2_panicking_retained_page_keeps_earlier_and_new_tokens_usable() {
    let (_root, s) = storage();
    let anchor = "a".repeat(300);
    create(
        &s,
        vec![
            entity(EntityKind::Team, &anchor),
            entity(EntityKind::Team, "z"),
        ],
    )
    .await;
    let earlier = token(&page(&s, &Filter::Teams, Sort::Id, None, 1).await, "02");
    let (send, recv) = std::sync::mpsc::channel();
    let other = s.clone();
    let panic = tokio::spawn(async move {
        other
            .write(
                owner("p"),
                Lane::Entities,
                Box::new(move |u| {
                    u.create(entity(EntityKind::Team, "rolled-back"))?;
                    let first = u.list(&Filter::Teams, &request(Sort::Id, None, 1))?;
                    send.send(token(&first, "02")).unwrap();
                    panic!("synthetic panic after retained cursor issuance");
                }),
            )
            .await
    })
    .await;
    assert!(panic.unwrap_err().is_panic());
    // A fresh list and a later write must both still work on this adapter.
    assert_eq!(
        ids(&page(&s, &Filter::Teams, Sort::Id, None, 10).await),
        [anchor.as_str(), "z"]
    );
    create(&s, vec![entity(EntityKind::Team, "y")]).await;
    for cursor in [earlier, recv.recv().unwrap()] {
        let next = page(&s, &Filter::Teams, Sort::Id, Some(cursor), 10).await;
        assert_eq!(ids(&next), ["y", "z"], "panic must preserve retained table");
        assert!(next.next_cursor.is_none());
    }
    s.close().await.unwrap();
}

#[tokio::test]
async fn p_e4_p_l1_eviction_interrupts_a_retained_walk_without_refreshing_its_age() {
    let (_root, s) = storage();
    create(
        &s,
        vec![
            entity(EntityKind::Team, &"a".repeat(300)),
            entity(EntityKind::Team, "z"),
        ],
    )
    .await;
    let oldest = token(&page(&s, &Filter::Teams, Sort::Id, None, 1).await, "02");
    let second = token(&page(&s, &Filter::Teams, Sort::Id, None, 1).await, "02");
    // Every issue consumes one slot, even for the same anchor.
    for _ in 2..1024 {
        page(&s, &Filter::Teams, Sort::Id, None, 1).await;
    }
    assert_eq!(
        ids(&page(&s, &Filter::Teams, Sort::Id, Some(oldest.clone()), 10).await),
        ["z"]
    );
    let newest = token(&page(&s, &Filter::Teams, Sort::Id, None, 1).await, "02");
    let refused = s
        .read(owner("p"))
        .await
        .unwrap()
        .list(&Filter::Teams, &request(Sort::Id, Some(oldest), 10))
        .await;
    assert!(
        matches!(refused, Err(StoreError::InvalidCursor)),
        "evicted walk returns no partial page"
    );
    for cursor in [second, newest] {
        assert_eq!(
            ids(&page(&s, &Filter::Teams, Sort::Id, Some(cursor), 10).await),
            ["z"]
        );
    }
    s.close().await.unwrap();
}

#[tokio::test]
async fn p_e4_c4_p_s1_p_s2_reopen_refuses_held_tokens_even_when_handles_are_reissued() {
    for same_generation in [true, false] {
        let root = tempfile::tempdir().unwrap();
        let s = open(
            root.path(),
            StorageOptions {
                generation: "first".into(),
                ..Default::default()
            },
        );
        let mut cursors = Vec::new();
        for (id, arm) in [
            ("a".into(), "01"),
            ("b".repeat(300), "02"),
            ("c".repeat(70_000), "03"),
        ] {
            create(
                &s,
                vec![entity(EntityKind::Team, &id), entity(EntityKind::Team, "z")],
            )
            .await;
            // Each project would also work, but removing each fixture keeps the
            // same filter and allows new adapter handles to collide by number.
            let first = page(&s, &Filter::Teams, Sort::Id, None, 1).await;
            cursors.push(token(&first, arm));
            s.transaction(owner("p"), Lane::Entities, move |u| {
                u.delete(EntityKind::Team, &id, Revision::ONE)?;
                u.delete(EntityKind::Team, "z", Revision::ONE)
            })
            .await
            .unwrap();
        }
        s.close().await.unwrap();
        drop(s);
        let s = open(
            root.path(),
            StorageOptions {
                generation: if same_generation { "first" } else { "second" }.into(),
                ..Default::default()
            },
        );
        create(
            &s,
            vec![
                entity(EntityKind::Team, &"a".repeat(300)),
                entity(EntityKind::Team, "z"),
            ],
        )
        .await;
        token(&page(&s, &Filter::Teams, Sort::Id, None, 1).await, "02");
        token(&page(&s, &Filter::Teams, Sort::Id, None, 1).await, "02");
        for (index, cursor) in cursors.into_iter().enumerate() {
            let result = s
                .read(owner("p"))
                .await
                .unwrap()
                .list(&Filter::Teams, &request(Sort::Id, Some(cursor), 10))
                .await;
            if same_generation && index == 0 {
                assert_eq!(ids(&result.unwrap()), ["a".repeat(300), "z".into()]);
            } else {
                assert!(
                    matches!(result, Err(StoreError::InvalidCursor)),
                    "reopen {same_generation} arm {index}"
                );
            }
        }
        s.close().await.unwrap();
    }
}

#[tokio::test]
async fn p_e4_i_t1_to_i_t3_q7_prime_inline_wall_clock_boundaries_return_no_page() {
    use orch_core::test_support::FixedClock;
    const NOW: i64 = 1_800_000_000_000;
    const DAY: i64 = 86_400_000;
    let root = tempfile::tempdir().unwrap();
    let clock = Arc::new(FixedClock::at(NOW));
    let s = open(
        root.path(),
        StorageOptions {
            clock: clock.clone(),
            ..Default::default()
        },
    );
    create(
        &s,
        vec![entity(EntityKind::Team, "a"), entity(EntityKind::Team, "z")],
    )
    .await;
    let cursor = token(&page(&s, &Filter::Teams, Sort::Id, None, 1).await, "01");
    for (delta, accepted) in [
        (DAY, true),
        (DAY + 1, false),
        (-300_000, true),
        (-300_001, false),
        (-30 * DAY, false),
        (30 * DAY, false),
    ] {
        clock.set_ms(NOW + delta);
        let result = s
            .read(owner("p"))
            .await
            .unwrap()
            .list(&Filter::Teams, &request(Sort::Id, Some(cursor.clone()), 10))
            .await;
        if accepted {
            assert_eq!(ids(&result.unwrap()), ["z"], "delta {delta}");
        } else {
            assert!(
                matches!(result, Err(StoreError::InvalidCursor)),
                "expired walk delta {delta}"
            );
        }
    }
    s.close().await.unwrap();
}

#[tokio::test]
async fn i_t4_inline_forged_expiry_obeys_the_future_claim_bound() {
    use orch_core::test_support::FixedClock;
    const NOW: i64 = 1_800_000_000_000;
    let root = tempfile::tempdir().unwrap();
    let s = open(
        root.path(),
        StorageOptions {
            clock: Arc::new(FixedClock::at(NOW)),
            ..Default::default()
        },
    );
    create(
        &s,
        vec![entity(EntityKind::Team, "a"), entity(EntityKind::Team, "z")],
    )
    .await;
    let cursor = token(&page(&s, &Filter::Teams, Sort::Id, None, 1).await, "01");
    for (extra, accepted) in [(0, true), (1, false)] {
        // ADR 0024 specifies the unauthenticated hex format and expiry bytes.
        let mut raw = cursor.as_str().to_owned();
        let expiry = (NOW + 86_400_000 + 300_000 + extra)
            .to_be_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        raw.replace_range(4..20, &expiry);
        let result = s
            .read(owner("p"))
            .await
            .unwrap()
            .list(
                &Filter::Teams,
                &request(Sort::Id, Some(Cursor::new(raw).unwrap()), 10),
            )
            .await;
        if accepted {
            assert_eq!(ids(&result.unwrap()), ["z"]);
        } else {
            assert!(matches!(result, Err(StoreError::InvalidCursor)));
        }
    }
    s.close().await.unwrap();
}

#[tokio::test]
async fn p_x3_all_three_arms_allow_limit_and_count_to_change() {
    for (suffix, arm) in [
        (String::new(), "01"),
        ("x".repeat(300), "02"),
        ("x".repeat(70_000), "03"),
    ] {
        let (_root, s) = storage();
        create(
            &s,
            vec![
                entity(EntityKind::Team, &format!("a{suffix}")),
                entity(EntityKind::Team, "y"),
                entity(EntityKind::Team, "z"),
            ],
        )
        .await;
        let first = s
            .read(owner("p"))
            .await
            .unwrap()
            .list(
                &Filter::Teams,
                &PageRequest {
                    with_count: false,
                    ..request(Sort::Id, None, 1)
                },
            )
            .await
            .unwrap();
        assert_eq!(first.total, None);
        let next = page(&s, &Filter::Teams, Sort::Id, Some(token(&first, arm)), 10).await;
        assert_eq!(ids(&next), ["y", "z"]);
        assert_eq!(next.total, Some(3));
        assert!(next.next_cursor.is_none());
        s.close().await.unwrap();
    }
}

fn capacity_cases() -> Vec<(Filter, Sort, usize)> {
    let mut cases = families()
        .into_iter()
        .map(|f| (f, Sort::Id, 230))
        .collect::<Vec<_>>();
    let task_runs = Filter::Runs {
        task: Some("t".into()),
        agent: None,
    };
    let pending = Filter::PendingMessages {
        recipient: "recipient".into(),
        now: "z".into(),
    };
    cases.extend([
        (task_runs.clone(), Sort::Id, 230),
        (pending.clone(), Sort::Id, 230),
    ]);
    cases.extend([
        (Filter::Context, Sort::Default, 230),
        (Filter::Config, Sort::Default, 230),
        (Filter::Templates, Sort::Default, 230),
        (Filter::Agents, Sort::Default, 222),
        (Filter::Agents, Sort::Name, 221),
        (Filter::Teams, Sort::Default, 229),
        (Filter::Teams, Sort::Name, 229),
        (
            Filter::Runs {
                task: None,
                agent: None,
            },
            Sort::Default,
            229,
        ),
        (task_runs.clone(), Sort::Default, 222),
        (task_runs, Sort::Attempt, 222),
        (Filter::Goals { status: None }, Sort::Default, 205),
        (Filter::Goals { status: None }, Sort::CreatedAt, 221),
        (Filter::Messages { recipient: None }, Sort::Default, 221),
        (pending, Sort::Default, 221),
    ]);
    cases
}

fn empty_order_entity(kind: EntityKind, id: &str) -> Entity {
    let mut e = entity(kind, id);
    match &mut e {
        Entity::Agent(a) => a.name.clear(),
        Entity::Team(t) => {
            t.name.clear();
            t.created_at.clear();
        }
        Entity::Run(r) => {
            r.started_at.clear();
            r.task_id = "t".into();
        }
        Entity::Message(m) => m.created_at.clear(),
        Entity::Goal(g) => {
            g.created_at.clear();
            g.updated_at = Some(String::new());
        }
        _ => {}
    }
    e
}

#[tokio::test]
async fn c7_p_b1_every_capacity_row_resumes_inline_and_retained_after_deletion() {
    for (f, sort, capacity) in capacity_cases() {
        for (extra, arm) in [(0, "01"), (1, "02")] {
            let (_root, s) = storage();
            let bytes = capacity + extra;
            let anchor = format!("{}{}", "é".repeat(bytes / 2), "x".repeat(bytes % 2));
            create(
                &s,
                vec![
                    empty_order_entity(f.kind(), &anchor),
                    empty_order_entity(f.kind(), "\u{10ffff}"),
                ],
            )
            .await;
            let first = page(&s, &f, sort, None, 1).await;
            assert_eq!(ids(&first), [anchor.as_str()], "{f:?} {sort:?}");
            let cursor = token(&first, arm);
            if extra == 0 {
                assert_eq!(cursor.as_str().len(), 512);
            }
            let kind = f.kind();
            s.transaction(owner("p"), Lane::Entities, move |u| {
                u.delete(kind, &anchor, Revision::ONE)
            })
            .await
            .unwrap();
            let next = page(&s, &f, sort, Some(cursor), 10).await;
            assert_eq!(ids(&next), ["\u{10ffff}"], "{f:?} {sort:?} {arm}");
            assert!(next.next_cursor.is_none());
            s.close().await.unwrap();
        }
    }
}

#[tokio::test]
async fn p_b2_p_b3_q1_q9_d3_a_d2_t_whole_tuple_boundaries_select_the_public_arm() {
    // Each case sets the exact physical texts named by the vector, then proves
    // unchanged lookup works and deleted lookup is refused (retained resumes).
    let mut cases = Vec::new();
    for (id, arm) in [
        ("a".repeat(65_536), "02"),
        ("a".repeat(65_537), "03"),
        ("é".repeat(32_768), "02"),
        (format!("{}a", "é".repeat(32_768)), "03"),
    ] {
        cases.push((
            Filter::Teams,
            Sort::Id,
            entity(EntityKind::Team, &id),
            entity(EntityKind::Team, "\u{10ffff}"),
            arm,
        ));
    }
    let agent = empty_order_entity(EntityKind::Agent, &"a".repeat(65_536));
    cases.push((
        Filter::Agents,
        Sort::Default,
        agent,
        empty_order_entity(EntityKind::Agent, "z"),
        "03",
    ));
    for (id_bytes, arm) in [(32_768, "02"), (32_769, "03")] {
        let mut e = entity(EntityKind::Team, &"a".repeat(id_bytes));
        if let Entity::Team(t) = &mut e {
            t.name = "a".repeat(32_768);
        }
        cases.push((
            Filter::Teams,
            Sort::Name,
            e,
            entity(EntityKind::Team, "z"),
            arm,
        ));
    }
    let mut agent = entity(EntityKind::Agent, &"a".repeat(40_000));
    if let Entity::Agent(a) = &mut agent {
        a.name = "a".repeat(40_000);
    }
    let mut successor = entity(EntityKind::Agent, "z");
    if let Entity::Agent(a) = &mut successor {
        a.name = "z".into();
    }
    cases.push((Filter::Agents, Sort::Name, agent, successor, "03"));
    for bytes in [40_000, 65_536] {
        cases.push((
            Filter::Goals { status: None },
            Sort::Default,
            goal(&"a".repeat(bytes), "", Some(&"z".repeat(bytes))),
            goal("next", "", Some("a")),
            "03",
        ));
    }
    for updated in [None, Some("")] {
        for (bytes, arm) in [(65_512, "02"), (65_513, "03")] {
            cases.push((
                Filter::Goals { status: None },
                Sort::Default,
                goal(&"a".repeat(bytes), "", updated),
                goal("next", "", updated),
                arm,
            ));
        }
    }
    for (f, sort, anchor, successor, arm) in cases {
        let (_root, s) = storage();
        let id = anchor.id().to_owned();
        let next_id = successor.id().to_owned();
        let kind = f.kind();
        create(&s, vec![anchor, successor]).await;
        let first = page(&s, &f, sort, None, 1).await;
        assert_eq!(ids(&first), [id.as_str()], "{f:?} {sort:?} {arm}");
        let cursor = token(&first, arm);
        assert_eq!(
            ids(&page(&s, &f, sort, Some(cursor.clone()), 10).await),
            [next_id.as_str()]
        );
        s.transaction(owner("p"), Lane::Entities, move |u| {
            u.delete(kind, &id, Revision::ONE)
        })
        .await
        .unwrap();
        let result = s
            .read(owner("p"))
            .await
            .unwrap()
            .list(&f, &request(sort, Some(cursor), 10))
            .await;
        if arm == "02" {
            assert_eq!(ids(&result.unwrap()), [next_id.as_str()]);
        } else {
            assert!(
                matches!(result, Err(StoreError::InvalidCursor)),
                "deleted lookup {f:?} {sort:?}"
            );
        }
        s.close().await.unwrap();
    }
}

#[tokio::test]
async fn c1_c2_retained_team_name_survives_anchor_and_two_deleted_successors() {
    // C1 ends on exactly 300 name bytes; C2 shares a 300-byte prefix.
    for prefix_bytes in [299, 300] {
        let (_root, s) = storage();
        let prefix = "a".repeat(prefix_bytes);
        let mut rows = Vec::new();
        for id in ["a", "b", "c", "d", "e"] {
            let mut row = entity(EntityKind::Team, id);
            if let Entity::Team(t) = &mut row {
                t.name = format!("{prefix}{id}");
            }
            rows.push(row);
        }
        create(&s, rows).await;
        let first = page(&s, &Filter::Teams, Sort::Name, None, 2).await;
        assert_eq!(ids(&first), ["a", "b"]);
        let cursor = token(&first, "02");
        s.transaction(owner("p"), Lane::Entities, |u| {
            for id in ["b", "c", "d"] {
                u.delete(EntityKind::Team, id, Revision::ONE)?;
            }
            Ok(())
        })
        .await
        .unwrap();
        let next = page(&s, &Filter::Teams, Sort::Name, Some(cursor), 10).await;
        assert_eq!(ids(&next), ["e"]);
        assert!(next.next_cursor.is_none());
        s.close().await.unwrap();
    }
}

#[tokio::test]
async fn p_r1_p_r2_q4_lookup_team_name_edits_refuse_instead_of_skipping_the_successor() {
    for change in ["rename", "recreate", "same-length"] {
        let (_root, s) = storage();
        let prefix = "a".repeat(60_000);
        let mut rows = Vec::new();
        for (id, name) in [
            ("before", format!("{prefix}a")),
            ("anchor", format!("{prefix}b{}", "x".repeat(69_999))),
            ("next", format!("{prefix}c")),
        ] {
            let mut row = entity(EntityKind::Team, id);
            if let Entity::Team(t) = &mut row {
                t.name = name;
            }
            rows.push(row);
        }
        create(&s, rows).await;
        let first = page(&s, &Filter::Teams, Sort::Name, None, 2).await;
        assert_eq!(ids(&first), ["before", "anchor"]);
        let cursor = token(&first, "03");
        s.transaction(owner("p"), Lane::Entities, move |u| {
            let row = u.get(EntityKind::Team, "anchor")?.unwrap();
            let Entity::Team(mut t) = row.entity else {
                unreachable!()
            };
            t.name = if change == "same-length" {
                "z".repeat(130_000)
            } else {
                "z".into()
            };
            if change == "recreate" {
                u.delete(EntityKind::Team, "anchor", row.revision)?;
                u.create(Entity::Team(t))?;
            } else {
                u.update(Entity::Team(t), row.revision)?;
            }
            Ok(())
        })
        .await
        .unwrap();
        let result = s
            .read(owner("p"))
            .await
            .unwrap()
            .list(&Filter::Teams, &request(Sort::Name, Some(cursor), 10))
            .await;
        assert!(
            matches!(result, Err(StoreError::InvalidCursor)),
            "{change} must not silently skip next"
        );
        s.close().await.unwrap();
    }
}

#[tokio::test]
async fn p_r3_q5_d1_s_lookup_matches_identity_and_allows_unrelated_edit_or_identical_recreation() {
    for recreate in [false, true] {
        let (_root, s) = storage();
        let anchor = "a".repeat(65_537);
        let other = "b".repeat(65_537);
        let mut rows = Vec::new();
        for (id, name) in [(&anchor, "a"), (&other, "b")] {
            let mut row = entity(EntityKind::Team, id);
            if let Entity::Team(t) = &mut row {
                t.name = name.into();
            }
            rows.push(row);
        }
        create(&s, rows).await;
        let first = page(&s, &Filter::Teams, Sort::Name, None, 1).await;
        assert_eq!(ids(&first), [anchor.as_str()]);
        let cursor = token(&first, "03");
        s.transaction(owner("p"), Lane::Entities, move |u| {
            for id in [anchor, other] {
                let row = u.get(EntityKind::Team, &id)?.unwrap();
                let Entity::Team(mut t) = row.entity else {
                    unreachable!()
                };
                if recreate {
                    u.delete(EntityKind::Team, &id, row.revision)?;
                    u.create(Entity::Team(t))?;
                } else {
                    t.task_pool.push("unrelated".into());
                    u.update(Entity::Team(t), row.revision)?;
                }
            }
            Ok(())
        })
        .await
        .unwrap();
        let next = page(&s, &Filter::Teams, Sort::Name, Some(cursor), 10).await;
        assert_eq!(ids(&next), ["b".repeat(65_537)]);
        assert!(next.next_cursor.is_none());
        s.close().await.unwrap();
    }
}

#[tokio::test]
async fn p_l2_public_byte_and_lookup_count_bounds_evict_exactly_the_oldest() {
    for lookup in [false, true] {
        let (_root, s) = storage();
        let (filter, sort, arm, bound, rows) = if lookup {
            (
                Filter::Goals { status: None },
                Sort::Default,
                "03",
                1024,
                vec![
                    goal(&"a".repeat(65_536), "", Some(&"z".repeat(65_536))),
                    goal("next", "", Some("a")),
                ],
            )
        } else {
            (
                Filter::Teams,
                Sort::Id,
                "02",
                128,
                vec![
                    entity(EntityKind::Team, &"a".repeat(65_536)),
                    entity(EntityKind::Team, "z"),
                ],
            )
        };
        create(&s, rows).await;
        let mut cursors = Vec::new();
        for _ in 0..bound {
            cursors.push(token(&page(&s, &filter, sort, None, 1).await, arm));
        }
        // Resume every entry: use must neither refresh age nor create a charge.
        for cursor in &cursors {
            assert_eq!(
                page(&s, &filter, sort, Some(cursor.clone()), 10)
                    .await
                    .items
                    .len(),
                1
            );
        }
        let newest = token(&page(&s, &filter, sort, None, 1).await, arm);
        let oldest = cursors.remove(0);
        let refused = s
            .read(owner("p"))
            .await
            .unwrap()
            .list(&filter, &request(sort, Some(oldest), 10))
            .await;
        assert!(
            matches!(refused, Err(StoreError::InvalidCursor)),
            "bound {bound} {arm}"
        );
        for cursor in cursors.into_iter().chain([newest]) {
            assert_eq!(
                page(&s, &filter, sort, Some(cursor), 10).await.items.len(),
                1,
                "only oldest evicted {arm}"
            );
        }
        s.close().await.unwrap();
    }
}

#[tokio::test]
async fn p_t3_edited_held_tokens_refuse_except_a_live_same_binding_handle() {
    let (_root, s) = storage();
    let a = "a".repeat(300);
    let b = "b".repeat(300);
    create(
        &s,
        vec![
            entity(EntityKind::Team, &a),
            entity(EntityKind::Team, &b),
            entity(EntityKind::Team, "z"),
        ],
    )
    .await;
    let first = page(&s, &Filter::Teams, Sort::Id, None, 1).await;
    let cursor = token(&first, "02");
    let second = page(&s, &Filter::Teams, Sort::Id, Some(cursor.clone()), 1).await;
    let second_token = token(&second, "02");
    let mut variants = vec![
        format!("{}00", cursor.as_str()),
        cursor.as_str()[..66].to_owned(),
    ];
    for offset in [0, 4, 36, 67] {
        // version, binding, nonce, missing handle
        let mut text = cursor.as_str().to_owned();
        let replacement = if &text[offset..offset + 1] == "f" {
            "e"
        } else {
            "f"
        };
        text.replace_range(offset..offset + 1, replacement);
        variants.push(text);
    }
    for text in variants {
        let result = s
            .read(owner("p"))
            .await
            .unwrap()
            .list(
                &Filter::Teams,
                &request(Sort::Id, Some(Cursor::new(text).unwrap()), 10),
            )
            .await;
        assert!(
            matches!(result, Err(StoreError::InvalidCursor)),
            "edited held token returns no page"
        );
    }
    // The token is not authenticated: swapping only the handle to a live entry
    // of this binding/sort deliberately resumes that entry (ADR 0024).
    let mut changed = cursor.as_str().to_owned();
    changed.replace_range(52..68, &second_token.as_str()[52..68]);
    let next = page(
        &s,
        &Filter::Teams,
        Sort::Id,
        Some(Cursor::new(changed).unwrap()),
        10,
    )
    .await;
    assert_eq!(ids(&next), ["z"]);
    s.close().await.unwrap();
}
