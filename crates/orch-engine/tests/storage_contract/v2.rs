//! Backend-neutral v2 entity scenarios. Register the file/SQLite factories here
//! when those slices arrive; expectations do not depend on the fake.

use std::sync::Arc;

use orch_core::store::fake::FakeStorage;
use orch_core::store::*;
use orch_domain::*;

use crate::contract::{STAMP, agent, goal, message, run, task, team};

pub struct Backend {
    pub storage: Arc<dyn Storage>,
    pub _guard: Box<dyn Send + Sync>,
}

pub fn memory_backend() -> Backend {
    Backend {
        storage: Arc::new(FakeStorage::default()),
        _guard: Box::new(()),
    }
}

fn owner(project: &str) -> Caller {
    Caller {
        project: project.into(),
        principal: Principal::Owner {
            identity: "owner".into(),
        },
    }
}

fn tasks() -> Filter {
    Filter::Tasks {
        status: None,
        goal: None,
    }
}

fn entity(id: &str) -> Entity {
    Entity::Task(Box::new(task(id)))
}

fn ids(page: &Page<Record>) -> Vec<&str> {
    page.items.iter().map(|r| r.entity.id()).collect()
}

async fn create(s: &dyn Storage, caller: Caller, value: Entity) -> Record {
    s.transaction(caller, Lane::Entities, move |u| u.create(value))
        .await
        .unwrap()
}

async fn list(
    s: &dyn Storage,
    caller: Caller,
    filter: &Filter,
    page: &PageRequest,
) -> Page<Record> {
    s.read(caller)
        .await
        .unwrap()
        .list(filter, page)
        .await
        .unwrap()
}

pub async fn revisions_and_positions_for_every_entity_family(b: Backend) {
    // Catches missing revision/position rules on a non-task entity family.
    let rows = [
        (
            Entity::Task(Box::new(task("b"))),
            Entity::Task(Box::new(task("a"))),
            tasks(),
        ),
        (
            Entity::Goal(Box::new(goal("b"))),
            Entity::Goal(Box::new(goal("a"))),
            Filter::Goals { status: None },
        ),
        (
            Entity::Agent(Box::new(agent("b"))),
            Entity::Agent(Box::new(agent("a"))),
            Filter::Agents,
        ),
        (
            Entity::Team(Box::new(team("b"))),
            Entity::Team(Box::new(team("a"))),
            Filter::Teams,
        ),
        (
            Entity::Run(Box::new(run("b", STAMP))),
            Entity::Run(Box::new(run("a", STAMP))),
            Filter::Runs {
                task: None,
                agent: None,
            },
        ),
        (
            Entity::Message(Box::new(message("b"))),
            Entity::Message(Box::new(message("a"))),
            Filter::Messages { recipient: None },
        ),
        (context("b"), context("a"), Filter::Context),
        (
            Entity::Config(ConfigEntry {
                key: "b".into(),
                value: "v".into(),
            }),
            Entity::Config(ConfigEntry {
                key: "a".into(),
                value: "v".into(),
            }),
            Filter::Config,
        ),
        (
            Entity::Template(TemplateEntry {
                name: "b".into(),
                body: "t".into(),
            }),
            Entity::Template(TemplateEntry {
                name: "a".into(),
                body: "t".into(),
            }),
            Filter::Templates,
        ),
    ];
    for (first, second, filter) in rows {
        let kind = first.kind();
        let saved = create(b.storage.as_ref(), owner("p"), first.clone()).await;
        let other = create(b.storage.as_ref(), owner("p"), second).await;
        assert_eq!(
            (saved.revision.get(), saved.position, other.position),
            (1, 1, 2)
        );
        b.storage
            .write(
                owner("p"),
                Lane::Entities,
                Box::new(move |u| {
                    let updated = u.update(first, Revision::ONE)?;
                    assert_eq!((updated.revision.get(), updated.position), (2, 1));
                    assert!(matches!(
                        u.delete(kind, "b", Revision::ONE),
                        Err(StoreError::StaleRevision { .. })
                    ));
                    let moved = u.move_to_end(kind, "b", updated.revision)?;
                    assert_eq!((moved.revision.get(), moved.position), (3, 3));
                    u.delete(kind, "a", other.revision)?;
                    assert_eq!(u.get(kind, "a")?, None);
                    Ok(())
                }),
            )
            .await
            .unwrap();
        let page = list(
            b.storage.as_ref(),
            owner("p"),
            &filter,
            &PageRequest::default(),
        )
        .await;
        assert_eq!(ids(&page), ["b"]);
        assert_eq!(page.items[0].revision.get(), 3);
    }
}

fn context(key: &str) -> Entity {
    Entity::Context(orch_core::ContextEntry {
        key: key.into(),
        value: key.into(),
        created_at: STAMP.into(),
        updated_at: STAMP.into(),
        ttl_ms: None,
        expires_at: None,
    })
}

pub async fn unit_of_work_keeps_nothing_after_an_error(b: Backend) {
    let s = b.storage.as_ref();
    create(s, owner("p"), entity("kept")).await;
    let result = s
        .write(
            owner("p"),
            Lane::Entities,
            Box::new(move |u| {
                u.update(
                    Entity::Task(Box::new(Task {
                        title: "lost".into(),
                        ..task("kept")
                    })),
                    Revision::ONE,
                )?;
                u.create(Entity::Agent(Box::new(agent("lost"))))?;
                u.delete(EntityKind::Task, "kept", Revision::ONE.next()?)?;
                Err(StoreError::InvalidPage("abort".into()))
            }),
        )
        .await;
    assert!(matches!(result, Err(StoreError::InvalidPage(_))));
    let snapshot = s.read(owner("p")).await.unwrap();
    assert_eq!(
        snapshot
            .get(EntityKind::Task, "kept")
            .await
            .unwrap()
            .unwrap()
            .entity,
        entity("kept")
    );
    assert_eq!(snapshot.get(EntityKind::Agent, "lost").await.unwrap(), None);
    let next = create(s, owner("p"), entity("next")).await;
    assert_eq!((next.revision.get(), next.position), (1, 2));
}

pub async fn stale_revision_is_refused_and_changes_nothing(b: Backend) {
    let s = b.storage.as_ref();
    create(s, owner("p"), entity("one")).await;
    s.write(
        owner("p"),
        Lane::Entities,
        Box::new(move |u| {
            u.update(
                Entity::Task(Box::new(Task {
                    title: "winner".into(),
                    ..task("one")
                })),
                Revision::ONE,
            )?;
            Ok(())
        }),
    )
    .await
    .unwrap();
    for operation in 0..3 {
        let result = s
            .write(
                owner("p"),
                Lane::Entities,
                Box::new(move |u| {
                    match operation {
                        0 => {
                            u.update(entity("one"), Revision::ONE)?;
                        }
                        1 => u.delete(EntityKind::Task, "one", Revision::ONE)?,
                        _ => {
                            u.move_to_end(EntityKind::Task, "one", Revision::ONE)?;
                        }
                    }
                    Ok(())
                }),
            )
            .await;
        assert!(
            matches!(result, Err(StoreError::StaleRevision { actual: Some(r), .. }) if r.get() == 2)
        );
    }
    let row = s
        .read(owner("p"))
        .await
        .unwrap()
        .get(EntityKind::Task, "one")
        .await
        .unwrap()
        .unwrap();
    assert_eq!((row.revision.get(), row.position), (2, 1));
    assert!(matches!(row.entity, Entity::Task(t) if t.title == "winner"));
}

pub async fn snapshot_keeps_one_committed_state(b: Backend) {
    let s = b.storage.as_ref();
    create(s, owner("p"), entity("a")).await;
    let old = s.read(owner("p")).await.unwrap();
    create(s, owner("p"), entity("b")).await;
    assert_eq!(
        ids(&old.list(&tasks(), &PageRequest::default()).await.unwrap()),
        ["a"]
    );
    assert_eq!(old.get(EntityKind::Task, "b").await.unwrap(), None);
    assert_eq!(
        ids(&list(s, owner("p"), &tasks(), &PageRequest::default()).await),
        ["a", "b"]
    );
}

pub async fn caller_selects_the_project_and_keeps_the_principal(b: Backend) {
    let s = b.storage.as_ref();
    for principal in [
        Principal::Owner {
            identity: "o".into(),
        },
        Principal::Worker {
            identity: "w".into(),
        },
        Principal::Task {
            identity: "i".into(),
            task: "t".into(),
            run: "r".into(),
            lineage: "restore-2".into(),
            epoch: 19,
        },
    ] {
        let caller = Caller {
            project: "other".into(),
            principal,
        };
        let snapshot = s.read(caller.clone()).await.unwrap();
        assert_eq!(snapshot.caller(), &caller);
        let expected = caller.clone();
        s.write(
            caller.clone(),
            Lane::Entities,
            Box::new(move |u| {
                assert_eq!(u.caller(), &expected);
                Ok(())
            }),
        )
        .await
        .unwrap();
    }
    create(s, owner("p"), entity("same")).await;
    assert_eq!(
        s.read(owner("other"))
            .await
            .unwrap()
            .get(EntityKind::Task, "same")
            .await
            .unwrap(),
        None
    );
    let second = create(s, owner("other"), entity("same")).await;
    assert_eq!(second.position, 1);
    s.write(
        owner("other"),
        Lane::Entities,
        Box::new(move |u| u.delete(EntityKind::Task, "same", Revision::ONE)),
    )
    .await
    .unwrap();
    assert!(
        s.read(owner("p"))
            .await
            .unwrap()
            .get(EntityKind::Task, "same")
            .await
            .unwrap()
            .is_some()
    );
}

pub async fn task_ascii_aliases_are_refused_without_rewriting_ids(b: Backend) {
    let s = b.storage.as_ref();
    create(s, owner("p"), entity("tsk_Case")).await;
    let alias = s
        .write(
            owner("p"),
            Lane::Entities,
            Box::new(move |u| {
                u.create(entity("tsk_case"))?;
                Ok(())
            }),
        )
        .await;
    assert!(
        matches!(alias, Err(StoreError::IdAlias { existing, requested }) if existing == "tsk_Case" && requested == "tsk_case")
    );
    let snapshot = s.read(owner("p")).await.unwrap();
    assert_eq!(
        snapshot.get(EntityKind::Task, "tsk_case").await.unwrap(),
        None
    );
    assert_eq!(
        snapshot
            .get(EntityKind::Task, "tsk_Case")
            .await
            .unwrap()
            .unwrap()
            .entity
            .id(),
        "tsk_Case"
    );
    create(s, owner("other"), entity("tsk_case")).await;
    // ASCII-only folding must not conflate non-ASCII pairs.
    create(s, owner("p"), entity("tsk_Ä")).await;
    create(s, owner("p"), entity("tsk_ä")).await;
}

pub async fn competing_writes_compare_the_committed_revision(b: Backend) {
    let s = b.storage;
    create(s.as_ref(), owner("p"), entity("a")).await;
    let mut writes = tokio::task::JoinSet::new();
    for n in 0..8 {
        let s = s.clone();
        writes.spawn(async move {
            s.write(
                owner("p"),
                Lane::Entities,
                Box::new(move |u| {
                    u.update(
                        Entity::Task(Box::new(Task {
                            title: format!("writer-{n}"),
                            ..task("a")
                        })),
                        Revision::ONE,
                    )?;
                    Ok(())
                }),
            )
            .await
        });
    }
    let (mut granted, mut stale) = (0, 0);
    while let Some(result) = writes.join_next().await {
        match result.unwrap() {
            Ok(()) => granted += 1,
            Err(StoreError::StaleRevision { .. }) => stale += 1,
            Err(err) => panic!("unexpected error: {err}"),
        }
    }
    assert_eq!((granted, stale), (1, 7));
}

pub async fn deletion_never_reuses_a_position(b: Backend) {
    let s = b.storage.as_ref();
    create(s, owner("p"), entity("a")).await;
    create(s, owner("p"), entity("b")).await;
    s.write(
        owner("p"),
        Lane::Entities,
        Box::new(move |u| {
            u.delete(EntityKind::Task, "b", Revision::ONE)?;
            u.delete(EntityKind::Task, "a", Revision::ONE)?;
            Ok(())
        }),
    )
    .await
    .unwrap();
    assert_eq!(create(s, owner("p"), entity("c")).await.position, 3);
}

pub async fn duplicate_create_and_missing_mutations_are_unchanged(b: Backend) {
    let s = b.storage.as_ref();
    create(s, owner("p"), entity("one")).await;
    s.write(
        owner("p"),
        Lane::Entities,
        Box::new(move |u| {
            assert!(matches!(
                u.create(entity("one")),
                Err(StoreError::AlreadyExists { .. })
            ));
            assert!(matches!(
                u.update(entity("missing"), Revision::ONE),
                Err(StoreError::StaleRevision { actual: None, .. })
            ));
            assert!(matches!(
                u.delete(EntityKind::Task, "missing", Revision::ONE),
                Err(StoreError::StaleRevision { actual: None, .. })
            ));
            assert!(matches!(
                u.move_to_end(EntityKind::Task, "missing", Revision::ONE),
                Err(StoreError::StaleRevision { actual: None, .. })
            ));
            let rows = u.list(&tasks(), &PageRequest::default())?;
            assert_eq!(ids(&rows), ["one"]);
            assert_eq!(
                (rows.items[0].revision.get(), rows.items[0].position),
                (1, 1)
            );
            Ok(())
        }),
    )
    .await
    .unwrap();
    assert_eq!(create(s, owner("p"), entity("two")).await.position, 2);
}

macro_rules! entity_suite {
    ($name:ident, $factory:path) => {
        mod $name {
            use crate::v2;
            macro_rules! case {
                ($test:ident) => {
                    #[tokio::test]
                    async fn $test() {
                        v2::$test($factory()).await;
                    }
                };
            }
            case!(revisions_and_positions_for_every_entity_family);
            case!(unit_of_work_keeps_nothing_after_an_error);
            case!(stale_revision_is_refused_and_changes_nothing);
            case!(snapshot_keeps_one_committed_state);
            case!(caller_selects_the_project_and_keeps_the_principal);
            case!(task_ascii_aliases_are_refused_without_rewriting_ids);
            case!(page_bounds_defaults_and_optional_count);
            case!(cursor_walk_is_stable_under_inserts_and_deletes);
            case!(cursor_is_bound_to_project_filter_and_sort);
            case!(list_sorts_and_filters_match_the_reference);
            case!(competing_writes_compare_the_committed_revision);
            case!(deletion_never_reuses_a_position);
            case!(duplicate_create_and_missing_mutations_are_unchanged);
            case!(declared_secondary_sorts_use_values_then_tie_breakers);
            case!(message_and_run_filters_stay_within_their_family);
            case!(settings_and_templates_are_keyed_rows_listed_by_their_exact_key);
            case!(settings_and_templates_belong_to_their_project);
            case!(a_failed_unit_of_work_keeps_no_setting_or_template_row_or_position);
            case!(text_over_64_kib_round_trips_through_the_blob_store);
            case!(text_over_the_limit_stays_whole_when_rows_projects_and_aborted_work_share_it);
            case!(two_task_ids_that_differ_only_in_case_are_refused);
            case!(task_lookups_stay_exact_to_the_byte);
            case!(only_task_ids_fold_and_only_the_ascii_letters);
        }
    };
}
pub(crate) use entity_suite;

#[path = "v2_paging.rs"]
mod paging;
pub use paging::{
    cursor_is_bound_to_project_filter_and_sort, cursor_walk_is_stable_under_inserts_and_deletes,
    declared_secondary_sorts_use_values_then_tie_breakers,
    list_sorts_and_filters_match_the_reference, message_and_run_filters_stay_within_their_family,
    page_bounds_defaults_and_optional_count,
};

#[path = "v2_settings.rs"]
mod settings;
pub use settings::{
    a_failed_unit_of_work_keeps_no_setting_or_template_row_or_position,
    only_task_ids_fold_and_only_the_ascii_letters,
    settings_and_templates_are_keyed_rows_listed_by_their_exact_key,
    settings_and_templates_belong_to_their_project, task_lookups_stay_exact_to_the_byte,
    text_over_64_kib_round_trips_through_the_blob_store,
    text_over_the_limit_stays_whole_when_rows_projects_and_aborted_work_share_it,
    two_task_ids_that_differ_only_in_case_are_refused,
};
