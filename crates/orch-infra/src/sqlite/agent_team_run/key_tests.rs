// Pages that resume after a recorded ordering tuple (D2, plan step 3): the leaf
// half of the vector P-F2 of the spec's "Cursors of the eight families other
// than Task", for Agent, Team and Run. The cursor, the held tuple and the
// lookup arm are other steps, so none of those vectors is closed here.
// Synthetic data only.

fn key_agent(id: &str, name: &str) -> Entity {
    let mut a = agent();
    a.id = id.into();
    a.name = name.into();
    Entity::Agent(Box::new(a))
}
fn key_team(id: &str, name: &str, created_at: &str) -> Entity {
    let mut t = team();
    t.id = id.into();
    t.name = name.into();
    t.created_at = created_at.into();
    Entity::Team(Box::new(t))
}
fn key_run(id: &str, task: &str, agent: &str, attempt: u32, started_at: &str) -> Entity {
    let mut r = run();
    r.id = id.into();
    r.task_id = task.into();
    r.agent_id = agent.into();
    r.attempt = attempt;
    r.started_at = started_at.into();
    Entity::Run(Box::new(r))
}
fn key_text(text: &str) -> Cow<'static, str> {
    Cow::Owned(text.into())
}
fn key_ids(
    sql: &NativeSql<'_, '_>,
    filter: &Filter,
    sort: Sort,
    after: Option<&OrderKey<'_>>,
) -> Vec<String> {
    page_ids(
        &sql.select_after(selection(filter, sort), after, 1000)
            .unwrap(),
    )
}
fn key_all(sql: &NativeSql<'_, '_>, filter: &Filter, sort: Sort) -> Vec<String> {
    page_ids(&sql.select(selection(filter, sort), None, 1000).unwrap())
}

/// A walk by recorded tuples equals the walk by anchor records and the whole list,
/// at every page size, and every page counts all the matches.
fn key_walk(
    sql: &NativeSql<'_, '_>,
    filter: &Filter,
    sort: Sort,
    name: Option<&str>,
    member: Option<&str>,
) -> Vec<String> {
    let q = || NativeSelection {
        filter,
        sort,
        name,
        member,
    };
    let whole = sql.select(q(), None, 1000).unwrap();
    for limit in 1..=3 {
        let mut seen = Vec::new();
        let mut last: Option<Record> = None;
        loop {
            let key = last.as_ref().map(|r| q().key_of(r).unwrap());
            let by_key = sql.select_after(q(), key.as_ref(), limit).unwrap();
            let by_record = sql.select(q(), last.as_ref(), limit).unwrap();
            assert_eq!(by_key.items, by_record.items, "{sort:?} {limit}");
            assert_eq!(by_key.total, whole.total);
            seen.extend(page_ids(&by_key));
            if !by_key.has_more {
                break;
            }
            last = by_key.items.last().cloned();
        }
        assert_eq!(seen, page_ids(&whole), "{sort:?} {limit}");
    }
    page_ids(&whole)
}

#[test]
fn key_walks_equal_anchor_walks_in_every_order_of_agents_teams_and_runs() {
    let (_temp, data) = crate::sqlite::test_data();
    let disk = DiskBlobStore::open(data.root()).unwrap();
    let mut conn = native_conn();
    let tx = conn.transaction().unwrap();
    let mut sql = NativeSql::new(&tx, &disk, "p", 123);
    // Positions follow creation. Names tie, so the position and then the id decide.
    for (id, name) in [("b", "n"), ("a", "n"), ("c", "m"), ("d", "n")] {
        sql.create(key_agent(id, name)).unwrap();
    }
    for (id, name, at) in [
        ("t3", "x", "2026"),
        ("t1", "x", "2026"),
        ("t2", "y", "2025"),
        ("t4", "x", "2026"),
    ] {
        sql.create(key_team(id, name, at)).unwrap();
    }
    for (id, task, agent, attempt, at) in [
        ("r1", "k", "a", 3, "s2"),
        ("r2", "k", "b", 3, "s2"),
        ("r3", "k", "a", 1, "s1"),
        ("r4", "k", "a", 2, "s3"),
        ("r5", "j", "a", 5, "s2"),
    ] {
        sql.create(key_run(id, task, agent, attempt, at)).unwrap();
    }
    let agents = Filter::Agents;
    for (sort, name, expected) in [
        (Sort::Default, None, vec!["b", "a", "c", "d"]),
        (Sort::Name, None, vec!["c", "b", "a", "d"]),
        (Sort::Id, None, vec!["a", "b", "c", "d"]),
        (Sort::Name, Some("n"), vec!["b", "a", "d"]),
    ] {
        assert_eq!(
            key_walk(&sql, &agents, sort, name, None),
            expected,
            "{sort:?}"
        );
    }
    let teams = Filter::Teams;
    for (sort, expected) in [
        // created_at then id; by name, name then id.
        (Sort::Default, ["t2", "t1", "t3", "t4"]),
        (Sort::Name, ["t1", "t3", "t4", "t2"]),
        (Sort::Id, ["t1", "t2", "t3", "t4"]),
    ] {
        assert_eq!(
            key_walk(&sql, &teams, sort, None, None),
            expected,
            "{sort:?}"
        );
    }
    let started = Filter::Runs {
        task: None,
        agent: None,
    };
    // No task in the filter: started_at descending, then the id.
    assert_eq!(
        key_walk(&sql, &started, Sort::Default, None, None),
        ["r4", "r1", "r2", "r5", "r3"]
    );
    assert_eq!(
        key_walk(&sql, &started, Sort::Id, None, None),
        ["r1", "r2", "r3", "r4", "r5"]
    );
    // A task in the filter: attempt descending then the id, by default and by attempt.
    let by_task = Filter::Runs {
        task: Some("k".into()),
        agent: None,
    };
    for sort in [Sort::Default, Sort::Attempt] {
        assert_eq!(
            key_walk(&sql, &by_task, sort, None, None),
            ["r1", "r2", "r4", "r3"],
            "{sort:?}"
        );
    }
    assert_eq!(
        key_walk(&sql, &by_task, Sort::Id, None, None),
        ["r1", "r2", "r3", "r4"]
    );
    let by_agent = Filter::Runs {
        task: None,
        agent: Some("a".into()),
    };
    assert_eq!(
        key_walk(&sql, &by_agent, Sort::Default, None, None),
        ["r4", "r1", "r5", "r3"]
    );
}

#[test]
fn key_resume_after_a_deleted_anchor_and_deleted_tied_rows_is_exact() {
    let (_temp, data) = crate::sqlite::test_data();
    let disk = DiskBlobStore::open(data.root()).unwrap();
    let mut conn = native_conn();
    let tx = conn.transaction().unwrap();
    let mut sql = NativeSql::new(&tx, &disk, "p", 123);
    // Five teams of one created_at and one name: only the id decides.
    for id in ["e", "b", "c", "a", "d"] {
        sql.create(key_team(id, "same", "t")).unwrap();
    }
    let teams = Filter::Teams;
    for (sort, tuple) in [
        (
            Sort::Default,
            OrderKey::TeamCreated {
                created_at: key_text("t"),
                id: key_text("b"),
            },
        ),
        (
            Sort::Name,
            OrderKey::TeamName {
                name: key_text("same"),
                id: key_text("b"),
            },
        ),
        (Sort::Id, OrderKey::Id(key_text("b"))),
    ] {
        let first = sql.select(selection(&teams, sort), None, 2).unwrap();
        assert_eq!(page_ids(&first), ["a", "b"], "{sort:?}");
        let anchor = first.items.last().unwrap();
        assert_eq!(selection(&teams, sort).key_of(anchor).unwrap(), tuple);
        // The anchor and the next tied row are deleted. The record-taking entry
        // has no anchor left; the tuple still names its place.
        for id in ["b", "c"] {
            let revision = sql.get(Family::Team, id).unwrap().unwrap().revision;
            sql.delete(Family::Team, id, revision).unwrap();
        }
        let rest = sql
            .select_after(selection(&teams, sort), Some(&tuple), 10)
            .unwrap();
        assert_eq!(page_ids(&rest), ["d", "e"], "{sort:?}");
        assert_eq!((rest.total, rest.has_more), (3, false));
        for id in ["b", "c"] {
            sql.create(key_team(id, "same", "t")).unwrap();
        }
    }
}

#[test]
fn key_resume_from_a_changed_anchor_uses_its_original_tuple() {
    let (_temp, data) = crate::sqlite::test_data();
    let disk = DiskBlobStore::open(data.root()).unwrap();
    let mut conn = native_conn();
    let tx = conn.transaction().unwrap();
    let mut sql = NativeSql::new(&tx, &disk, "p", 123);
    for id in ["a", "b", "c", "d"] {
        sql.create(key_team(id, "n", "t")).unwrap();
    }
    let teams = Filter::Teams;
    let first = sql.select(selection(&teams, Sort::Name), None, 2).unwrap();
    let anchor = first.items.last().unwrap().clone();
    let tuple = OrderKey::TeamName {
        name: key_text("n"),
        id: key_text("b"),
    };
    assert_eq!(
        selection(&teams, Sort::Name).key_of(&anchor).unwrap(),
        tuple
    );
    // The anchor is renamed past every other row. The two rows that did not
    // change follow the original tuple, and the anchor is returned at its new place.
    sql.update(key_team("b", "z", "t"), anchor.revision)
        .unwrap();
    assert_eq!(
        key_ids(&sql, &teams, Sort::Name, Some(&tuple)),
        ["c", "d", "b"]
    );
    assert_eq!(key_all(&sql, &teams, Sort::Name), ["a", "c", "d", "b"]);
}

#[test]
fn key_resume_from_a_tuple_that_names_no_row_is_by_bytes_not_by_locale() {
    let (_temp, data) = crate::sqlite::test_data();
    let disk = DiskBlobStore::open(data.root()).unwrap();
    let mut conn = native_conn();
    let tx = conn.transaction().unwrap();
    let mut sql = NativeSql::new(&tx, &disk, "p", 123);
    // "Z" < "a" < "a1" < "é" (0xC3 0xA9): a prefix sorts before its extension.
    for (id, name) in [("e", "é"), ("f", "a"), ("g", "Z"), ("h", "a1")] {
        sql.create(key_team(id, name, "t")).unwrap();
    }
    let teams = Filter::Teams;
    assert_eq!(key_all(&sql, &teams, Sort::Name), ["g", "f", "h", "e"]);
    let at = |name: &str, id: &str| OrderKey::TeamName {
        name: key_text(name),
        id: key_text(id),
    };
    for (key, expected) in [
        (at("", ""), vec!["g", "f", "h", "e"]),
        (at("Z", "zzz"), vec!["f", "h", "e"]),
        (at("a", "e"), vec!["f", "h", "e"]),
        (at("a", "f"), vec!["h", "e"]),
        (at("a0", ""), vec!["h", "e"]),
        (at("é", "e"), vec![]),
    ] {
        assert_eq!(
            key_ids(&sql, &teams, Sort::Name, Some(&key)),
            expected,
            "{key:?}"
        );
    }
    // A run's started_at and attempt descend: the tuple names a place there too.
    for (id, attempt, at) in [("r1", 1, "b"), ("r2", 2, "a"), ("r3", 3, "c")] {
        sql.create(key_run(id, "k", "a", attempt, at)).unwrap();
    }
    let by_task = Filter::Runs {
        task: Some("k".into()),
        agent: None,
    };
    let attempt = |attempt| OrderKey::RunAttempt {
        attempt,
        id: key_text(""),
    };
    assert_eq!(
        key_ids(&sql, &by_task, Sort::Default, Some(&attempt(u32::MAX))),
        ["r3", "r2", "r1"]
    );
    assert_eq!(
        key_ids(&sql, &by_task, Sort::Default, Some(&attempt(2))),
        ["r2", "r1"]
    );
    assert_eq!(
        key_ids(&sql, &by_task, Sort::Default, Some(&attempt(0))),
        Vec::<String>::new()
    );
    let all = Filter::Runs {
        task: None,
        agent: None,
    };
    let started = |at: &str| OrderKey::RunStarted {
        started_at: key_text(at),
        id: key_text(""),
    };
    assert_eq!(
        key_ids(&sql, &all, Sort::Default, Some(&started("d"))),
        ["r3", "r1", "r2"]
    );
    assert_eq!(
        key_ids(&sql, &all, Sort::Default, Some(&started("b"))),
        ["r1", "r2"]
    );
    assert_eq!(
        key_ids(&sql, &all, Sort::Default, Some(&started("A"))),
        Vec::<String>::new()
    );
}

#[test]
fn key_identity_order_is_bytes_not_characters() {
    let (_temp, data) = crate::sqlite::test_data();
    let disk = DiskBlobStore::open(data.root()).unwrap();
    let mut conn = native_conn();
    let tx = conn.transaction().unwrap();
    let mut sql = NativeSql::new(&tx, &disk, "p", 123);
    // "é" is one character of two bytes: by bytes it sorts after "b", and by
    // characters it would sort before the two-character ids.
    for id in ["b", "é", "ab", "a1"] {
        sql.create(key_agent(id, "n")).unwrap();
    }
    let agents = Filter::Agents;
    assert_eq!(key_all(&sql, &agents, Sort::Id), ["a1", "ab", "b", "é"]);
    for (at, expected) in [
        ("a1", vec!["ab", "b", "é"]),
        ("a2", vec!["ab", "b", "é"]),
        ("ab", vec!["b", "é"]),
        ("b", vec!["é"]),
        ("é", vec![]),
    ] {
        let key = OrderKey::Id(key_text(at));
        assert_eq!(
            key_ids(&sql, &agents, Sort::Id, Some(&key)),
            expected,
            "{at}"
        );
    }
}

#[test]
fn key_resume_does_not_check_the_filter_so_an_anchor_that_left_it_still_names_its_place() {
    // P-F2, leaf half: runs filtered by agent; the anchor is deleted, or changed
    // so that it no longer matches, and the walk continues exactly.
    let (_temp, data) = crate::sqlite::test_data();
    let disk = DiskBlobStore::open(data.root()).unwrap();
    let mut conn = native_conn();
    let tx = conn.transaction().unwrap();
    let mut sql = NativeSql::new(&tx, &disk, "p", 123);
    for (id, agent, at) in [
        ("r1", "a", "s5"),
        ("r2", "a", "s4"),
        ("r3", "other", "s3"),
        ("r4", "a", "s2"),
        ("r5", "a", "s1"),
    ] {
        sql.create(key_run(id, "k", agent, 1, at)).unwrap();
    }
    let filter = Filter::Runs {
        task: None,
        agent: Some("a".into()),
    };
    let first = sql
        .select(selection(&filter, Sort::Default), None, 2)
        .unwrap();
    assert_eq!(page_ids(&first), ["r1", "r2"]);
    let anchor = first.items.last().unwrap().clone();
    let tuple = OrderKey::RunStarted {
        started_at: key_text("s4"),
        id: key_text("r2"),
    };
    assert_eq!(
        selection(&filter, Sort::Default).key_of(&anchor).unwrap(),
        tuple
    );
    // Changed to another agent: it leaves the filter and keeps its ordering values.
    let moved = sql
        .update(key_run("r2", "k", "other", 1, "s4"), anchor.revision)
        .unwrap();
    assert!(matches!(
        sql.select(selection(&filter, Sort::Default), Some(&moved), 10),
        Err(NativeError::Store(StoreError::InvalidCursor))
    ));
    let rest = sql
        .select_after(selection(&filter, Sort::Default), Some(&tuple), 10)
        .unwrap();
    assert_eq!(page_ids(&rest), ["r4", "r5"]);
    // The total counts what matches now, before the cursor.
    assert_eq!((rest.total, rest.has_more), (3, false));
    // Deleted: no record is left, and the result does not change.
    sql.delete(Family::Run, "r2", moved.revision).unwrap();
    assert_eq!(
        key_ids(&sql, &filter, Sort::Default, Some(&tuple)),
        ["r4", "r5"]
    );
    // An agent, a team member and a name filter are no different.
    let agents = Filter::Agents;
    for (id, name) in [("a1", "n"), ("a2", "n"), ("a3", "n")] {
        sql.create(key_agent(id, name)).unwrap();
    }
    let by_name = |name| NativeSelection {
        filter: &agents,
        sort: Sort::Default,
        name,
        member: None,
    };
    let anchor = sql
        .select(by_name(Some("n")), None, 1)
        .unwrap()
        .items
        .remove(0);
    let tuple = OrderKey::AgentPosition {
        position: anchor.position,
        id: key_text("a1"),
    };
    assert_eq!(by_name(Some("n")).key_of(&anchor).unwrap(), tuple);
    sql.update(key_agent("a1", "renamed"), anchor.revision)
        .unwrap();
    assert_eq!(
        page_ids(
            &sql.select_after(by_name(Some("n")), Some(&tuple), 10)
                .unwrap()
        ),
        ["a2", "a3"]
    );
    let with_member = |id: &str, member: &str| {
        let Entity::Team(mut t) = key_team(id, "n", "t") else {
            unreachable!()
        };
        t.members = serde_json::from_value(
            json!([{"agent_id": member, "role": "member", "joined_at": "x"}]),
        )
        .unwrap();
        Entity::Team(t)
    };
    let mut records = Vec::new();
    for id in ["t1", "t2", "t3"] {
        records.push(sql.create(with_member(id, "m1")).unwrap());
    }
    let teams = Filter::Teams;
    let by_member = || NativeSelection {
        filter: &teams,
        sort: Sort::Id,
        name: None,
        member: Some("m1"),
    };
    let tuple = OrderKey::Id(key_text("t2"));
    assert_eq!(by_member().key_of(&records[1]).unwrap(), tuple);
    sql.update(with_member("t2", "m2"), records[1].revision)
        .unwrap();
    assert_eq!(
        page_ids(&sql.select_after(by_member(), Some(&tuple), 10).unwrap()),
        ["t3"]
    );
}

#[test]
fn key_resume_refuses_a_tuple_of_another_shape_and_validates_limit_and_selection_first() {
    let (_temp, data) = crate::sqlite::test_data();
    let disk = DiskBlobStore::open(data.root()).unwrap();
    let mut conn = native_conn();
    let tx = conn.transaction().unwrap();
    let sql = NativeSql::new(&tx, &disk, "p", 123);
    let id = OrderKey::Id(key_text("x"));
    let agent_position = OrderKey::AgentPosition {
        position: 1,
        id: key_text("x"),
    };
    let agent_name = OrderKey::AgentName {
        name: key_text("n"),
        position: 1,
        id: key_text("x"),
    };
    let team_created = OrderKey::TeamCreated {
        created_at: key_text("t"),
        id: key_text("x"),
    };
    let team_name = OrderKey::TeamName {
        name: key_text("n"),
        id: key_text("x"),
    };
    let run_started = OrderKey::RunStarted {
        started_at: key_text("s"),
        id: key_text("x"),
    };
    let run_attempt = OrderKey::RunAttempt {
        attempt: 1,
        id: key_text("x"),
    };
    let task_runs = Filter::Runs {
        task: Some("k".into()),
        agent: None,
    };
    let runs = Filter::Runs {
        task: None,
        agent: None,
    };
    // Each selection takes exactly one shape, and every other shape is refused.
    let shapes = [
        &id,
        &agent_position,
        &agent_name,
        &team_created,
        &team_name,
        &run_started,
        &run_attempt,
    ];
    let selections = [
        (&Filter::Agents, Sort::Id, 0),
        (&Filter::Teams, Sort::Id, 0),
        (&runs, Sort::Id, 0),
        (&Filter::Agents, Sort::Default, 1),
        (&Filter::Agents, Sort::Name, 2),
        (&Filter::Teams, Sort::Default, 3),
        (&Filter::Teams, Sort::Name, 4),
        (&runs, Sort::Default, 5),
        (&task_runs, Sort::Default, 6),
        (&task_runs, Sort::Attempt, 6),
    ];
    for (filter, sort, accepted) in selections {
        for (i, shape) in shapes.iter().enumerate() {
            let result = sql.select_after(selection(filter, sort), Some(shape), 1);
            if i == accepted {
                assert!(result.is_ok(), "{filter:?} {sort:?} {shape:?}");
            } else {
                assert!(
                    matches!(result, Err(NativeError::Store(StoreError::InvalidCursor))),
                    "{filter:?} {sort:?} {shape:?}"
                );
            }
        }
    }
    for limit in [0, 1001] {
        assert!(matches!(
            sql.select_after(
                selection(&Filter::Agents, Sort::Default),
                Some(&team_name),
                limit
            ),
            Err(NativeError::Store(StoreError::InvalidPage(_)))
        ));
    }
    // An unsupported sort or family is `InvalidPage`, whatever the tuple.
    assert!(matches!(
        sql.select_after(selection(&runs, Sort::Name), Some(&id), 1),
        Err(NativeError::Store(StoreError::InvalidPage(_)))
    ));
    assert!(matches!(
        sql.select_after(selection(&Filter::Context, Sort::Id), None, 1),
        Err(NativeError::Store(StoreError::InvalidPage(_)))
    ));
}

#[test]
fn key_hydration_is_in_the_view_the_page_is_selected_from() {
    let (_temp, data) = crate::sqlite::test_data();
    let disk = DiskBlobStore::open(data.root()).unwrap();
    let path = _temp.path().join("key-view.sqlite");
    let mut writer = rusqlite::Connection::open(&path).unwrap();
    writer.execute_batch("PRAGMA journal_mode=WAL").unwrap();
    for schema in [
        crate::sqlite::migrations::INITIAL_SCHEMA,
        crate::sqlite::migrations::ATTENTION_FACTS,
        crate::sqlite::migrations::TASK_FORMAT,
        crate::sqlite::migrations::AGENT_TEAM_RUN,
    ] {
        writer.execute_batch(schema).unwrap();
    }
    {
        let tx = writer.transaction().unwrap();
        let mut sql = NativeSql::new(&tx, &disk, "p", 123);
        for (id, name) in [("a", "same"), ("b", "zzz")] {
            sql.create(key_agent(id, name)).unwrap();
        }
        tx.commit().unwrap();
    }
    let mut reader = rusqlite::Connection::open(&path).unwrap();
    let snapshot = reader.transaction().unwrap();
    let key = OrderKey::AgentName {
        name: key_text("same"),
        position: 1,
        id: key_text("a"),
    };
    let select = |tx: &Transaction<'_>| {
        NativeSql::new(tx, &disk, "p", 123)
            .select_after(selection(&Filter::Agents, Sort::Name), Some(&key), 10)
            .unwrap()
    };
    let before = select(&snapshot);
    assert_eq!(page_ids(&before), ["b"]);
    // A later commit renames the anchor's row, deletes the next and adds another.
    {
        let tx = writer.transaction().unwrap();
        let mut sql = NativeSql::new(&tx, &disk, "p", 123);
        let a = sql.get(Family::Agent, "a").unwrap().unwrap();
        sql.update(key_agent("a", "new"), a.revision).unwrap();
        let b = sql.get(Family::Agent, "b").unwrap().unwrap();
        sql.delete(Family::Agent, "b", b.revision).unwrap();
        sql.create(key_agent("c", "zz")).unwrap();
        // The caller's own transaction sees its uncommitted state.
        assert_eq!(page_ids(&select(&tx)), ["c"]);
        tx.commit().unwrap();
    }
    let still = select(&snapshot);
    assert_eq!(still.items, before.items);
    assert_eq!((still.total, still.has_more), (2, false));
    snapshot.commit().unwrap();
    let fresh = reader.transaction().unwrap();
    let now = select(&fresh);
    assert_eq!(page_ids(&now), ["c"]);
    // Only the rows that match now are counted: the renamed row sorts before the tuple.
    assert_eq!(now.total, 2);
}

#[test]
fn key_resume_hydrates_every_candidate_so_damage_anywhere_fails_the_page() {
    let (_temp, data) = crate::sqlite::test_data();
    let disk = DiskBlobStore::open(data.root()).unwrap();
    let mut conn = native_conn();
    let tx = conn.transaction().unwrap();
    let mut sql = NativeSql::new(&tx, &disk, "p", 123);
    for id in ["a", "b", "c"] {
        sql.create(key_agent(id, id)).unwrap();
    }
    tx.execute("UPDATE agents SET name='drift' WHERE id='a'", [])
        .unwrap();
    // "a" sorts before the tuple and is not returned, but it is hydrated.
    let tuple = OrderKey::AgentPosition {
        position: 2,
        id: key_text("b"),
    };
    assert!(matches!(
        sql.select_after(selection(&Filter::Agents, Sort::Default), Some(&tuple), 10),
        Err(NativeError::Corrupt {
            family: "agent",
            ..
        })
    ));
}
