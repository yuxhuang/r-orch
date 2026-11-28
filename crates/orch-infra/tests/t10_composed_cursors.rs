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
    let storage = SqliteStorage::open(Arc::new(data), StorageOptions::default()).unwrap();
    (root, storage)
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
                for id in ["a", "e", "z"] {
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
            assert_eq!(next.total, Some(6));
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
