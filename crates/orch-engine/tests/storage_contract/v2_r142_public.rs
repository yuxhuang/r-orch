//! R142 public-port vectors, shared verbatim with SQLite's named integration cases.
//! Expectations use original UTF-8 text, never SQLite key forms or projections.
use orch_core::store::*;
use orch_core::test_support::{make_agent, make_run};
use orch_domain::*;

fn owner(project: &str) -> Caller {
    Caller {
        project: project.into(),
        principal: Principal::Owner {
            identity: "owner".into(),
        },
    }
}

fn runs(task: Option<&str>, agent: Option<&str>) -> Filter {
    Filter::Runs {
        task: task.map(str::to_owned),
        agent: agent.map(str::to_owned),
    }
}

fn run(id: &str, attempt: u32, task: &str, agent: &str, time: &str) -> Entity {
    let mut r = make_run();
    r.id = id.into();
    r.attempt = attempt;
    r.task_id = task.into();
    r.agent_id = agent.into();
    r.started_at = time.into();
    Entity::Run(Box::new(r))
}

async fn create(s: &dyn Storage, entity: Entity) {
    s.transaction(owner("p"), Lane::Entities, move |u| u.create(entity))
        .await
        .unwrap();
}

fn ids(page: &Page<Record>) -> Vec<&str> {
    page.items.iter().map(|r| r.entity.id()).collect()
}

pub async fn runs_of_one_task_with_attempts_2_10_10_list_numerically_descending_with_binary_id_ties(
    s: &dyn Storage,
) {
    // Lexical attempt order, insertion order and parsed-instant time order all disagree.
    for (id, attempt, task, agent, time) in [
        ("z", 10, "task", "agent", "2025-01-01T01:00:00+01:00"),
        ("A", 10, "task", "agent", "2025-01-01T00:00:00Z"),
        ("b", 2, "task", "agent", "opaque-z"),
        ("other-task", 99, "other", "agent", "opaque-a"),
        ("other-agent", 11, "task", "other", "opaque-b"),
    ] {
        create(s, run(id, attempt, task, agent, time)).await;
    }
    let snapshot = s.read(owner("p")).await.unwrap();
    for sort in [Sort::Default, Sort::Attempt] {
        let page = snapshot
            .list(
                &runs(Some("task"), Some("agent")),
                &PageRequest {
                    sort,
                    with_count: true,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(ids(&page), ["A", "z", "b"]);
        assert_eq!(page.total, Some(3));
        let page = snapshot
            .list(
                &runs(Some("task"), None),
                &PageRequest {
                    sort,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(ids(&page), ["other-agent", "A", "z", "b"]);
    }
    for filter in [runs(None, None), runs(None, Some("agent"))] {
        let page = snapshot
            .list(&filter, &PageRequest::default())
            .await
            .unwrap();
        let expected = if matches!(&filter, Filter::Runs { agent: Some(_), .. }) {
            vec!["b", "other-task", "z", "A"]
        } else {
            vec!["b", "other-agent", "other-task", "z", "A"]
        };
        assert_eq!(ids(&page), expected);
    }
}

pub async fn filters_pages_and_cursors_agree_with_the_reference_for_all_three_families(
    s: &dyn Storage,
) {
    // This long original sorts BEFORE inline neighbours. A form-major index puts it last.
    let long = format!("a{}", "λ".repeat(32768));
    let long_id = format!("a{}", "i".repeat(65536));
    let long_time = format!("a{}", "t".repeat(65536));
    let mut agents = Vec::new();
    let mut teams = Vec::new();
    let mut run_rows = Vec::new();
    for (id, name) in [(long_id.as_str(), long.as_str()), ("b", "b"), ("c", "c")] {
        let mut a = make_agent();
        a.id = id.into();
        a.name = name.into();
        agents.push(Entity::Agent(Box::new(a)));
        teams.push(Entity::Team(Box::new(Team {
            id: id.into(),
            name: name.into(),
            description: Some(long.clone()),
            status: TeamStatus::Active,
            members: vec![TeamMember {
                agent_id: long_id.clone(),
                role: TeamMemberRole::Member,
                joined_at: long_time.clone(),
            }],
            task_pool: vec![long.clone(), "task".into(), long.clone()],
            lead_agent_id: long_id.clone(),
            created_at: long_time.clone(),
            updated_at: name.into(),
            config: DEFAULT_TEAM_CONFIG,
        })));
        run_rows.push(run(
            id,
            2,
            &long,
            &long_id,
            if id == long_id { &long_time } else { name },
        ));
    }
    for row in agents.iter().chain(&teams).chain(&run_rows) {
        create(s, row.clone()).await;
    }
    for (filter, sort, expected) in [
        (Filter::Agents, Sort::Name, agents),
        (Filter::Teams, Sort::Name, teams),
        (
            runs(Some(&long), Some(&long_id)),
            Sort::Default,
            run_rows.clone(),
        ),
        (
            runs(None, Some(&long_id)),
            Sort::Default,
            run_rows.into_iter().rev().collect(),
        ),
    ] {
        let snapshot = s.read(owner("p")).await.unwrap();
        let request = PageRequest {
            limit: 1,
            sort,
            with_count: true,
            ..Default::default()
        };
        let first = snapshot.list(&filter, &request).await.unwrap();
        assert_eq!(first.items[0].entity, expected[0]);
        assert_eq!(first.total, Some(3));
        let next = PageRequest {
            cursor: Some(first.next_cursor.clone().unwrap()),
            ..request.clone()
        };
        assert!(matches!(
            s.read(owner("q")).await.unwrap().list(&filter, &next).await,
            Err(StoreError::InvalidCursor)
        ));
        let wrong_family = if filter == Filter::Agents {
            Filter::Teams
        } else {
            Filter::Agents
        };
        assert!(matches!(
            snapshot.list(&wrong_family, &next).await,
            Err(StoreError::InvalidCursor)
        ));
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
        if let Filter::Runs { .. } = &filter {
            for changed in [
                runs(Some("wrong"), Some(&long_id)),
                runs(Some(&long), Some("wrong")),
                runs(None, None),
            ] {
                assert!(matches!(
                    snapshot.list(&changed, &next).await,
                    Err(StoreError::InvalidCursor)
                ));
            }
        }
        let mut page_request = next.clone();
        for (i, entity) in expected.iter().enumerate().skip(1) {
            let page = snapshot.list(&filter, &page_request).await.unwrap();
            assert_eq!(page.items[0].entity, *entity);
            assert_eq!(page.total, Some(3));
            if i == 2 {
                assert!(page.next_cursor.is_none());
            }
            page_request.cursor = page.next_cursor;
        }
    }
    // The deletion guarantee is exact for retained tuples. These names share
    // a multibyte 300-byte prefix; anchor and two successors disappear together.
    let prefix = "λ".repeat(150);
    for index in 0..7 {
        let mut a = make_agent();
        a.id = format!("retained-{index}");
        a.name = format!("{prefix}{index}");
        s.transaction(owner("retained"), Lane::Entities, move |u| {
            u.create(Entity::Agent(Box::new(a)))
        })
        .await
        .unwrap();
    }
    let request = PageRequest {
        limit: 2,
        sort: Sort::Name,
        with_count: true,
        ..Default::default()
    };
    let first = s
        .read(owner("retained"))
        .await
        .unwrap()
        .list(&Filter::Agents, &request)
        .await
        .unwrap();
    s.transaction(owner("retained"), Lane::Entities, |u| {
        for index in [1, 2, 3] {
            u.delete(
                EntityKind::Agent,
                &format!("retained-{index}"),
                Revision::ONE,
            )?;
        }
        Ok(())
    })
    .await
    .unwrap();
    let after = s
        .read(owner("retained"))
        .await
        .unwrap()
        .list(
            &Filter::Agents,
            &PageRequest {
                cursor: first.next_cursor,
                ..request
            },
        )
        .await
        .unwrap();
    assert_eq!(
        after
            .items
            .iter()
            .map(|r| r.entity.id())
            .collect::<Vec<_>>(),
        ["retained-4", "retained-5"]
    );
    assert_eq!(after.total, Some(4));
    // Update indirect data to DIFFERENT bytes; observing the first payload is insufficient.
    for kind in [EntityKind::Agent, EntityKind::Team, EntityKind::Run] {
        let row = s
            .read(owner("p"))
            .await
            .unwrap()
            .get(kind, &long_id)
            .await
            .unwrap()
            .unwrap();
        let mut changed = row.entity;
        match &mut changed {
            Entity::Agent(a) => a.name = format!("0{}", "ν".repeat(32768)),
            Entity::Team(t) => t.members[0].joined_at = format!("0{}", "ν".repeat(32768)),
            Entity::Run(r) => r.prompt = format!("0{}", "ν".repeat(32768)),
            _ => unreachable!(),
        }
        let expected = changed.clone();
        s.transaction(owner("p"), Lane::Entities, move |u| {
            u.update(changed, row.revision)
        })
        .await
        .unwrap();
        assert_eq!(
            s.read(owner("p"))
                .await
                .unwrap()
                .get(kind, &long_id)
                .await
                .unwrap()
                .unwrap()
                .entity,
            expected
        );
    }
}
