//! Settings, templates, the 64 KiB text threshold and the task-id fold rule,
//! shared by every v2 registration. Nothing here names a backend: a row is
//! reached only through the port, and the expectations come from the spec
//! (R8, R9, R10, R11, R15, R97, R117), not from what a double happens to do.

use super::*;

/// R97: text up to this many UTF-8 bytes may stay in the row; one byte more
/// goes behind the blob store. The scenarios round-trip both sides of it.
const INLINE_LIMIT: usize = 64 * 1024;

fn setting(key: &str, value: &str) -> Entity {
    Entity::Config(ConfigEntry {
        key: key.into(),
        value: value.into(),
    })
}

fn template(name: &str, body: &str) -> Entity {
    Entity::Template(TemplateEntry {
        name: name.into(),
        body: body.into(),
    })
}

async fn get(s: &dyn Storage, caller: Caller, kind: EntityKind, id: &str) -> Option<Record> {
    s.read(caller).await.unwrap().get(kind, id).await.unwrap()
}

async fn refused(
    s: &dyn Storage,
    caller: Caller,
    filter: &Filter,
    page: PageRequest,
) -> StoreError {
    s.read(caller)
        .await
        .unwrap()
        .list(filter, &page)
        .await
        .unwrap_err()
}

pub async fn settings_and_templates_are_keyed_rows_listed_by_their_exact_key(b: Backend) {
    let s = b.storage.as_ref();
    // R99: template names are opaque UTF-8 strings. These are T-V4–T-V12,
    // including every T-V11 variant, plus a storage-only embedded NUL key.
    // This explicit byte-order oracle is independent of the backend's sort.
    let exact_keys = [
        "".to_owned(),            // T-V4
        "\u{1b}[0m".to_owned(),   // T-V10
        " sp ace ".to_owned(),    // T-V6
        ".".to_owned(),           // T-V5
        "..".to_owned(),          // T-V5
        "A".to_owned(),           // T-V7
        "a".to_owned(),           // T-V7
        "a\0b".to_owned(),        // storage only, no filename analogue
        "a\nb".to_owned(),        // T-V9
        "a\\b".to_owned(),        // T-V11e
        "e\u{301}".to_owned(),    // T-V8, decomposed
        "x".repeat(252),          // T-V12
        "\u{7f}".to_owned(),      // T-V11a
        "\u{85}".to_owned(),      // T-V11b
        "é".to_owned(),           // T-V8, composed
        "\u{202e}rtl".to_owned(), // T-V11c
        "\u{feff}x".to_owned(),   // T-V11d
    ];
    // Reverse insertion makes position order disagree with exact key order.
    for key in exact_keys.iter().rev() {
        create(s, owner("utf8"), template(key, key)).await;
    }
    for key in &exact_keys {
        assert_eq!(
            get(s, owner("utf8"), EntityKind::Template, key)
                .await
                .unwrap()
                .entity,
            template(key, key),
            "exact template lookup {key:?}"
        );
    }
    let mut cursor = None;
    // A bounded walk fails on an extra/repeated page instead of looping forever.
    for (index, key) in exact_keys.iter().enumerate() {
        let page = list(
            s,
            owner("utf8"),
            &Filter::Templates,
            &PageRequest {
                sort: Sort::Id,
                limit: 1,
                cursor: cursor.take(),
                ..Default::default()
            },
        )
        .await;
        assert_eq!(ids(&page), [key.as_str()], "template page {index}");
        if index + 1 == exact_keys.len() {
            assert_eq!(page.next_cursor, None, "last template page terminates");
        } else {
            cursor = Some(page.next_cursor.expect("another exact key remains"));
        }
    }

    // Written out of order, in pairs that byte order and a case-folding order
    // would split differently: `Run_Cap` and `run_cap` are two rows, and so
    // are `é` and `É`. Only task ids fold (R117).
    let keys = [
        "run_cap",
        "Run_Cap",
        "preapproval",
        "é",
        "É",
        "default",
        "legacy.config",
    ];
    s.write(
        owner("p"),
        Lane::Entities,
        Box::new(move |u| {
            for key in keys {
                u.create(setting(key, key))?;
                u.create(template(key, key))?;
            }
            // Other families may share a name and must not show in these lists.
            u.create(context("default"))?;
            u.create(entity("default"))?;
            Ok(())
        }),
    )
    .await
    .unwrap();
    let expected = [
        "Run_Cap",
        "default",
        "legacy.config",
        "preapproval",
        "run_cap",
        "É",
        "é",
    ];
    for filter in [Filter::Config, Filter::Templates] {
        let page = list(
            s,
            owner("p"),
            &filter,
            &PageRequest {
                with_count: true,
                ..Default::default()
            },
        )
        .await;
        assert_eq!(ids(&page), expected, "{filter:?}");
        assert_eq!(page.total, Some(7), "{filter:?}");
        let by_id = list(
            s,
            owner("p"),
            &filter,
            &PageRequest {
                sort: Sort::Id,
                ..Default::default()
            },
        )
        .await;
        assert_eq!(ids(&by_id), expected, "{filter:?} by id");
        // A walk through a list by cursor meets every row once (R10).
        let mut walked = vec![];
        let mut cursor = None;
        loop {
            let page = list(
                s,
                owner("p"),
                &filter,
                &PageRequest {
                    limit: 3,
                    cursor: cursor.take(),
                    ..Default::default()
                },
            )
            .await;
            walked.extend(ids(&page).into_iter().map(str::to_owned));
            match page.next_cursor {
                Some(next) => cursor = Some(next),
                None => break,
            }
        }
        assert_eq!(walked, expected, "{filter:?} walked");
        for sort in [
            Sort::Position,
            Sort::Name,
            Sort::Priority,
            Sort::CreatedAt,
            Sort::UpdatedAt,
        ] {
            assert!(
                matches!(
                    refused(
                        s,
                        owner("p"),
                        &filter,
                        PageRequest {
                            sort,
                            ..Default::default()
                        }
                    )
                    .await,
                    StoreError::InvalidPage(_)
                ),
                "{filter:?} {sort:?}"
            );
        }
    }
    // A cursor belongs to the filter it was issued for, whichever family.
    let first = list(
        s,
        owner("p"),
        &Filter::Config,
        &PageRequest {
            limit: 2,
            ..Default::default()
        },
    )
    .await;
    assert!(matches!(
        refused(
            s,
            owner("p"),
            &Filter::Templates,
            PageRequest {
                cursor: first.next_cursor,
                ..Default::default()
            }
        )
        .await,
        StoreError::InvalidCursor
    ));
    // Every lookup is exact to the byte, and each family has its own rows.
    for (kind, id, found) in [
        (EntityKind::Config, "run_cap", true),
        (EntityKind::Config, "RUN_CAP", false),
        (EntityKind::Config, "run_cap ", false),
        (EntityKind::Template, "é", true),
        (EntityKind::Template, "e\u{301}", false),
        (EntityKind::Template, "legacy.config", true),
        (EntityKind::Config, "missing", false),
    ] {
        let row = get(s, owner("p"), kind, id).await;
        assert_eq!(row.is_some(), found, "{kind:?} {id:?}");
        if let Some(row) = row {
            assert_eq!(row.entity.kind(), kind);
            assert_eq!(row.entity.id(), id);
        }
    }
    // Each family counts its own positions (R11), so a template does not
    // consume a setting's position.
    let run_cap = get(s, owner("p"), EntityKind::Config, "run_cap")
        .await
        .unwrap();
    let t_run_cap = get(s, owner("p"), EntityKind::Template, "run_cap")
        .await
        .unwrap();
    assert_eq!((run_cap.position, t_run_cap.position), (1, 1));
    assert_eq!(
        create(s, owner("p"), setting("last", "v")).await.position,
        8
    );
}

pub async fn settings_and_templates_belong_to_their_project(b: Backend) {
    let s = b.storage.as_ref();
    create(s, owner("p"), setting("run_cap", "2")).await;
    create(s, owner("p"), template("default", "mine")).await;
    // The same key in another project is a new row: revision 1, position 1.
    for (kind, value) in [
        (EntityKind::Config, "run_cap"),
        (EntityKind::Template, "default"),
    ] {
        assert_eq!(get(s, owner("other"), kind, value).await, None, "{kind:?}");
    }
    let other_setting = create(s, owner("other"), setting("run_cap", "9")).await;
    let other_template = create(s, owner("other"), template("default", "theirs")).await;
    assert_eq!(
        (other_setting.revision.get(), other_setting.position),
        (1, 1)
    );
    assert_eq!(
        (other_template.revision.get(), other_template.position),
        (1, 1)
    );
    // An update and a delete in one project do not reach the other (R15).
    s.write(
        owner("other"),
        Lane::Entities,
        Box::new(move |u| {
            u.update(setting("run_cap", "10"), Revision::ONE)?;
            u.delete(EntityKind::Template, "default", Revision::ONE)
        }),
    )
    .await
    .unwrap();
    let mine = get(s, owner("p"), EntityKind::Config, "run_cap")
        .await
        .unwrap();
    assert_eq!(mine.revision.get(), 1);
    assert_eq!(mine.entity, setting("run_cap", "2"));
    assert_eq!(
        get(s, owner("p"), EntityKind::Template, "default")
            .await
            .unwrap()
            .entity,
        template("default", "mine")
    );
    assert_eq!(
        ids(&list(
            s,
            owner("other"),
            &Filter::Templates,
            &PageRequest::default()
        )
        .await),
        Vec::<&str>::new()
    );
    assert_eq!(
        get(s, owner("other"), EntityKind::Config, "run_cap")
            .await
            .unwrap()
            .entity,
        setting("run_cap", "10")
    );
    // A deleted row's position is not handed out again, and not in the other
    // project's count either (R11).
    assert_eq!(
        create(s, owner("other"), template("again", "t"))
            .await
            .position,
        2
    );
    assert_eq!(
        create(s, owner("p"), template("second", "t"))
            .await
            .position,
        2
    );
}

pub async fn a_failed_unit_of_work_keeps_no_setting_or_template_row_or_position(b: Backend) {
    let s = b.storage.as_ref();
    create(s, owner("p"), setting("kept", "old")).await;
    create(s, owner("p"), template("kept", "old")).await;
    let result = s
        .write(
            owner("p"),
            Lane::Entities,
            Box::new(move |u| {
                u.update(setting("kept", "new"), Revision::ONE)?;
                u.create(setting("lost", "x"))?;
                u.create(template("lost", "x"))?;
                u.move_to_end(EntityKind::Template, "kept", Revision::ONE)?;
                u.delete(EntityKind::Config, "kept", Revision::ONE.next()?)?;
                Err(StoreError::InvalidPage("abort".into()))
            }),
        )
        .await;
    assert!(matches!(result, Err(StoreError::InvalidPage(_))));
    let kept = get(s, owner("p"), EntityKind::Config, "kept")
        .await
        .unwrap();
    assert_eq!((kept.revision.get(), kept.position), (1, 1));
    assert_eq!(kept.entity, setting("kept", "old"));
    let kept = get(s, owner("p"), EntityKind::Template, "kept")
        .await
        .unwrap();
    assert_eq!((kept.revision.get(), kept.position), (1, 1));
    assert_eq!(kept.entity, template("kept", "old"));
    for (kind, id) in [(EntityKind::Config, "lost"), (EntityKind::Template, "lost")] {
        assert_eq!(get(s, owner("p"), kind, id).await, None, "{kind:?}");
    }
    // The aborted work used no position; a refused create inside work that
    // commits uses none either, so the next two rows are 2 and 3 with no gap.
    s.write(
        owner("p"),
        Lane::Entities,
        Box::new(move |u| {
            assert!(matches!(
                u.create(setting("kept", "again")),
                Err(StoreError::AlreadyExists {
                    kind: EntityKind::Config,
                    ..
                })
            ));
            assert!(matches!(
                u.create(template("kept", "again")),
                Err(StoreError::AlreadyExists {
                    kind: EntityKind::Template,
                    ..
                })
            ));
            assert_eq!(u.create(setting("next", "v"))?.position, 2);
            assert_eq!(u.create(template("next", "v"))?.position, 2);
            Ok(())
        }),
    )
    .await
    .unwrap();
    assert_eq!(
        create(s, owner("p"), setting("after", "v")).await.position,
        3
    );
}

/// One row of each family that carries free text, holding `text`.
fn rows_holding(text: &str, id: &str) -> [Entity; 5] {
    [
        Entity::Message(Box::new(Message {
            body: text.into(),
            ..message(id)
        })),
        Entity::Context(orch_core::ContextEntry {
            key: id.into(),
            value: text.into(),
            created_at: STAMP.into(),
            updated_at: STAMP.into(),
            ttl_ms: None,
            expires_at: None,
        }),
        setting(id, text),
        template(id, text),
        Entity::Task(Box::new(Task {
            description: text.into(),
            ..task(id)
        })),
    ]
}

fn text_of(entity: &Entity) -> &str {
    match entity {
        Entity::Message(v) => &v.body,
        Entity::Context(v) => &v.value,
        Entity::Config(v) => &v.value,
        Entity::Template(v) => &v.body,
        Entity::Task(v) => &v.description,
        _ => unreachable!("rows_holding builds no other family"),
    }
}

fn filter_of(kind: EntityKind) -> Filter {
    match kind {
        EntityKind::Message => Filter::Messages { recipient: None },
        EntityKind::Context => Filter::Context,
        EntityKind::Config => Filter::Config,
        EntityKind::Template => Filter::Templates,
        EntityKind::Task => tasks(),
        other => unreachable!("rows_holding builds no {other:?}"),
    }
}

/// Texts on both sides of the 64 KiB line, counted in bytes. A character
/// count would put `é` and `€` on the wrong side.
fn texts_around_the_limit() -> Vec<(&'static str, String)> {
    let hostile = "\"\\\n\0\u{2028}/~0 ~1 ".repeat(INLINE_LIMIT / 10 + 1);
    vec![
        ("empty", String::new()),
        ("one byte", "x".into()),
        ("limit - 1", "a".repeat(INLINE_LIMIT - 1)),
        ("limit", "a".repeat(INLINE_LIMIT)),
        ("limit + 1", "a".repeat(INLINE_LIMIT + 1)),
        ("2-byte, limit", "é".repeat(INLINE_LIMIT / 2)),
        (
            "2-byte, limit + 1 with fewer characters",
            format!("{}a", "é".repeat(INLINE_LIMIT / 2)),
        ),
        (
            "3-byte, limit",
            format!("{}a", "€".repeat(INLINE_LIMIT / 3)),
        ),
        ("3-byte, limit + 2", "€".repeat(INLINE_LIMIT / 3 + 1)),
        ("4-byte, limit", "😀".repeat(INLINE_LIMIT / 4)),
        (
            "4-byte, limit + 1",
            format!("{}a", "😀".repeat(INLINE_LIMIT / 4)),
        ),
        ("quotes, NUL and separators", hostile),
        ("1 MiB and 3 bytes", "z".repeat(1024 * 1024 + 3)),
    ]
}

pub async fn text_over_64_kib_round_trips_through_the_blob_store(b: Backend) {
    let s = b.storage.as_ref();
    for (name, text) in texts_around_the_limit() {
        let rows = rows_holding(&text, "row");
        let kinds: Vec<EntityKind> = rows.iter().map(Entity::kind).collect();
        let written = rows.clone();
        s.write(
            owner("p"),
            Lane::Entities,
            Box::new(move |u| {
                for row in written {
                    u.create(row)?;
                }
                Ok(())
            }),
        )
        .await
        .unwrap();
        // The text comes back whole by `get` and by `list`, and the row beside
        // it is unchanged, for every family that carries text.
        let snapshot = s.read(owner("p")).await.unwrap();
        for (kind, row) in kinds.iter().copied().zip(&rows) {
            let got = snapshot.get(kind, "row").await.unwrap().unwrap();
            assert_eq!(&got.entity, row, "{name}: {kind:?} by get");
            assert_eq!(text_of(&got.entity), text, "{name}: {kind:?} text");
            let page = snapshot
                .list(&filter_of(kind), &PageRequest::default())
                .await
                .unwrap();
            assert_eq!(page.items.len(), 1, "{name}: {kind:?} by list");
            assert_eq!(&page.items[0].entity, row, "{name}: {kind:?} by list");
        }
        // An update to other text, over or under the line, replaces it, and the
        // snapshot taken before still holds the first.
        let other = if text.len() > INLINE_LIMIT {
            "short".to_owned()
        } else {
            "y".repeat(INLINE_LIMIT + 7)
        };
        let replaced = rows_holding(&other, "row");
        s.write(
            owner("p"),
            Lane::Entities,
            Box::new(move |u| {
                for row in replaced {
                    u.update(row, Revision::ONE)?;
                }
                Ok(())
            }),
        )
        .await
        .unwrap();
        for kind in &kinds {
            let now = get(s, owner("p"), *kind, "row").await.unwrap();
            assert_eq!(text_of(&now.entity), other, "{name}: {kind:?} after update");
            assert_eq!(now.revision.get(), 2, "{name}: {kind:?}");
            let before = snapshot.get(*kind, "row").await.unwrap().unwrap();
            assert_eq!(
                text_of(&before.entity),
                text,
                "{name}: {kind:?} old snapshot"
            );
        }
        s.write(
            owner("p"),
            Lane::Entities,
            Box::new(move |u| {
                for kind in kinds {
                    u.delete(kind, "row", Revision::ONE.next()?)?;
                }
                Ok(())
            }),
        )
        .await
        .unwrap();
    }
}

pub async fn text_over_the_limit_stays_whole_when_rows_projects_and_aborted_work_share_it(
    b: Backend,
) {
    let s = b.storage.as_ref();
    let text = "shared ".repeat(INLINE_LIMIT / 7 + 1);
    assert!(text.len() > INLINE_LIMIT);
    // Identical text in two rows and in another project is one blob in a
    // content-addressed store. Removing a row, or abandoning work that wrote the
    // same text, must not take it from a row that still holds it.
    for id in ["a", "b"] {
        for row in rows_holding(&text, id) {
            create(s, owner("p"), row).await;
        }
    }
    for row in rows_holding(&text, "a") {
        create(s, owner("other"), row).await;
    }
    let aborted = rows_holding(&text, "c");
    let result = s
        .write(
            owner("p"),
            Lane::Entities,
            Box::new(move |u| {
                for row in aborted {
                    u.create(row)?;
                }
                Err(StoreError::InvalidPage("abort".into()))
            }),
        )
        .await;
    assert!(matches!(result, Err(StoreError::InvalidPage(_))));
    let kinds: Vec<EntityKind> = rows_holding("", "a").iter().map(Entity::kind).collect();
    for kind in &kinds {
        assert_eq!(get(s, owner("p"), *kind, "c").await, None, "{kind:?}");
    }
    s.write(
        owner("p"),
        Lane::Entities,
        Box::new({
            let kinds = kinds.clone();
            move |u| {
                for kind in kinds {
                    u.delete(kind, "a", Revision::ONE)?;
                }
                Ok(())
            }
        }),
    )
    .await
    .unwrap();
    for kind in &kinds {
        let second = get(s, owner("p"), *kind, "b").await.unwrap();
        assert_eq!(text_of(&second.entity), text, "{kind:?} second row");
        let other = get(s, owner("other"), *kind, "a").await.unwrap();
        assert_eq!(text_of(&other.entity), text, "{kind:?} other project");
        assert_eq!(get(s, owner("p"), *kind, "a").await, None, "{kind:?}");
    }
    // The text can be written again after the rows that held it are gone.
    for row in rows_holding(&text, "a") {
        let again = create(s, owner("p"), row).await;
        assert_eq!(text_of(&again.entity), text);
    }
}

pub async fn two_task_ids_that_differ_only_in_case_are_refused(b: Backend) {
    let s = b.storage.as_ref();
    create(s, owner("p"), entity("tsk_Case")).await;
    let alias = s
        .write(
            owner("p"),
            Lane::Entities,
            Box::new(move |u| u.create(entity("tsk_case")).map(drop)),
        )
        .await;
    assert!(
        matches!(&alias, Err(StoreError::IdAlias { existing, requested }) if existing == "tsk_Case" && requested == "tsk_case"),
        "{alias:?}"
    );
    // One byte of the id differs beyond case: a new task, and so is the same
    // id in another project.
    create(s, owner("p"), entity("tsk_cas3")).await;
    create(s, owner("other"), entity("tsk_case")).await;
    assert_eq!(get(s, owner("p"), EntityKind::Task, "tsk_case").await, None);
    // The ids stand as written, and the refusal took no position (R11).
    assert_eq!(
        ids(&list(s, owner("p"), &tasks(), &PageRequest::default()).await),
        ["tsk_Case", "tsk_cas3"]
    );
    let positions: Vec<u64> = list(s, owner("p"), &tasks(), &PageRequest::default())
        .await
        .items
        .iter()
        .map(|row| row.position)
        .collect();
    assert_eq!(positions, [1, 2]);
}

pub async fn task_lookups_stay_exact_to_the_byte(b: Backend) {
    let s = b.storage.as_ref();
    create(s, owner("p"), entity("tsk_Case")).await;
    s.write(
        owner("p"),
        Lane::Entities,
        Box::new(move |u| {
            // Another spelling finds nothing: a read, an update, a delete and a
            // move are each a miss, never the stored row and never an alias.
            for id in ["tsk_case", "TSK_CASE", "tsk_CASE"] {
                assert_eq!(u.get(EntityKind::Task, id)?, None, "{id}");
                assert!(matches!(
                    u.update(entity(id), Revision::ONE),
                    Err(StoreError::StaleRevision { actual: None, .. })
                ));
                assert!(matches!(
                    u.delete(EntityKind::Task, id, Revision::ONE),
                    Err(StoreError::StaleRevision { actual: None, .. })
                ));
                assert!(matches!(
                    u.move_to_end(EntityKind::Task, id, Revision::ONE),
                    Err(StoreError::StaleRevision { actual: None, .. })
                ));
            }
            // A refused create changes nothing, so work that goes on commits
            // the rest and the next position follows without a gap.
            assert!(matches!(
                u.create(entity("tsk_case")),
                Err(StoreError::IdAlias { .. })
            ));
            assert_eq!(u.create(entity("tsk_next"))?.position, 2);
            // Inside one unit of work the second spelling is refused as well.
            assert!(matches!(
                u.create(entity("TSK_NEXT")),
                Err(StoreError::IdAlias { existing, requested }) if existing == "tsk_next" && requested == "TSK_NEXT"
            ));
            Ok(())
        }),
    )
    .await
    .unwrap();
    // The stored id was never rewritten, and the row is reached by it alone.
    let row = get(s, owner("p"), EntityKind::Task, "tsk_Case")
        .await
        .unwrap();
    assert_eq!((row.entity.id(), row.revision.get()), ("tsk_Case", 1));
    // Deleting a task frees its fold key, in this unit of work and the next.
    s.write(
        owner("p"),
        Lane::Entities,
        Box::new(move |u| {
            u.delete(EntityKind::Task, "tsk_Case", Revision::ONE)?;
            let reused = u.create(entity("tsk_case"))?;
            assert_eq!(reused.position, 3);
            assert!(matches!(
                u.create(entity("tsk_CASE")),
                Err(StoreError::IdAlias { existing, .. }) if existing == "tsk_case"
            ));
            Ok(())
        }),
    )
    .await
    .unwrap();
    assert_eq!(
        ids(&list(
            s,
            owner("p"),
            &tasks(),
            &PageRequest {
                sort: Sort::Position,
                ..Default::default()
            }
        )
        .await),
        ["tsk_next", "tsk_case"]
    );
}

pub async fn only_task_ids_fold_and_only_the_ascii_letters(b: Backend) {
    let s = b.storage.as_ref();
    // The other families keep ids that differ only in case (R117 is about
    // tasks, whose ids name a workspace and a branch).
    s.write(
        owner("p"),
        Lane::Entities,
        Box::new(move |u| {
            for id in ["Id", "id"] {
                u.create(Entity::Goal(Box::new(goal(id))))?;
                u.create(Entity::Agent(Box::new(agent(id))))?;
                u.create(Entity::Team(Box::new(team(id))))?;
                u.create(Entity::Message(Box::new(message(id))))?;
                u.create(Entity::Run(Box::new(run(id, STAMP))))?;
                u.create(context(id))?;
                u.create(setting(id, id))?;
                u.create(template(id, id))?;
            }
            Ok(())
        }),
    )
    .await
    .unwrap();
    // `tsk_k` and `tsk_K` fold; the Kelvin sign, the dotless i and the other
    // letters outside `A-Z` do not, whatever a locale or Unicode says. The
    // rule is the same for every backend and every platform.
    for (first, second) in [
        ("tsk_k", "tsk_\u{212a}"),
        ("tsk_I", "tsk_\u{131}"),
        ("tsk_\u{c4}", "tsk_\u{e4}"),
        ("tsk_[", "tsk_{"),
        ("tsk_@", "tsk_`"),
    ] {
        create(s, owner("p"), entity(first)).await;
        create(s, owner("p"), entity(second)).await;
    }
    for (first, second) in [
        ("tsk_a", "tsk_A"),
        ("tsk_Z", "tsk_z"),
        ("tsk_aZ0", "tsk_Az0"),
    ] {
        create(s, owner("p"), entity(first)).await;
        let result = s
            .write(
                owner("p"),
                Lane::Entities,
                Box::new(move |u| u.create(entity(second)).map(drop)),
            )
            .await;
        assert!(
            matches!(&result, Err(StoreError::IdAlias { existing, .. }) if existing == first),
            "{first} {second}: {result:?}"
        );
    }
}
