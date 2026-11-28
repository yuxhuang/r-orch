//! Issue 441: caller representability is a typed, content-free refusal on every port.
//! Synthetic fixtures only. SQLite failures remain active until its public mapping is wired.
use orch_core::storage::ContextEntry;
use orch_core::store::{
    Caller, Entity, EntityKind, Lane, Principal, Record, Storage, StorageExt, StoreError,
    fake::FakeStorage,
};
use orch_core::test_support::make_task;
use orch_infra::data_dir::{DataDir, Holder, LockKind};
use orch_infra::sqlite::{
    ConnectionRole, FatalCause, Role, SqliteStorage, StorageOptions, Synchronous, open_connection,
};
use orch_infra::storage::v2::FileStorage;
use std::sync::Arc;

fn owner() -> Caller {
    Caller {
        project: "p".into(),
        principal: Principal::Owner {
            identity: "synthetic".into(),
        },
    }
}

fn task(id: &str) -> Entity {
    let mut task = make_task();
    task.id = id.into();
    task.created_at = "2026-01-01T00:00:00.000Z".into();
    task.updated_at = task.created_at.clone();
    Entity::Task(Box::new(task))
}

fn context() -> Entity {
    Entity::Context(ContextEntry {
        key: "context".into(),
        value: "synthetic".into(),
        created_at: "opaque".into(),
        updated_at: "opaque".into(),
        ttl_ms: None,
        expires_at: None,
    })
}

#[derive(Clone, Copy)]
enum Adapter {
    Memory,
    File,
    Sqlite,
}

struct Fixture {
    root: tempfile::TempDir,
    data: Option<Arc<DataDir>>,
    storage: Box<dyn Storage>,
    adapter: Adapter,
}

impl Fixture {
    fn new(adapter: Adapter) -> Self {
        let root = tempfile::tempdir().unwrap();
        let data = matches!(adapter, Adapter::Sqlite).then(|| {
            Arc::new(
                DataDir::acquire(
                    &root.path().join("data"),
                    &Holder {
                        kind: LockKind::Daemon,
                        pid: std::process::id(),
                        started_at: "synthetic".into(),
                        binary_version: "test".into(),
                        executable: "/synthetic/orch".into(),
                    },
                )
                .unwrap(),
            )
        });
        let storage: Box<dyn Storage> = match adapter {
            Adapter::Memory => Box::new(FakeStorage::default()),
            Adapter::File => Box::new(FileStorage::new(root.path().join("file"))),
            Adapter::Sqlite => Box::new(
                SqliteStorage::open(data.clone().unwrap(), StorageOptions::default()).unwrap(),
            ),
        };
        Self {
            root,
            data,
            storage,
            adapter,
        }
    }

    async fn get(&self, kind: EntityKind, id: &str) -> Option<Record> {
        self.storage
            .read(owner())
            .await
            .unwrap()
            .get(kind, id)
            .await
            .unwrap()
    }

    async fn create(&self, entity: Entity) -> Record {
        self.storage
            .transaction(owner(), Lane::Entities, move |unit| unit.create(entity))
            .await
            .unwrap()
    }

    // Drop the old owned writer and reader pool, then open the same durable data.
    async fn reopen(&mut self) {
        assert!(!matches!(self.adapter, Adapter::Memory));
        let old = std::mem::replace(&mut self.storage, Box::new(FakeStorage::default()));
        drop(old);
        self.storage = match self.adapter {
            Adapter::File => Box::new(FileStorage::new(self.root.path().join("file"))),
            Adapter::Sqlite => Box::new(
                SqliteStorage::open(self.data.clone().unwrap(), StorageOptions::default()).unwrap(),
            ),
            Adapter::Memory => unreachable!(),
        };
    }
}

fn refusal(error: StoreError) {
    assert!(
        matches!(error, StoreError::Unrepresentable),
        "expected typed input refusal, got {error:?}"
    );
    assert_eq!(error.to_string(), "input cannot be represented by storage");
    assert_eq!(format!("{error:?}"), "Unrepresentable");
    assert!(std::error::Error::source(&error).is_none());
}

#[derive(Clone, Copy)]
enum Field {
    Id,
    Assignee,
    Goal,
    Created,
    Updated,
}

fn set(entity: &mut Entity, field: Field, value: &str) {
    let Entity::Task(task) = entity else {
        unreachable!()
    };
    match field {
        Field::Id => task.id = value.into(),
        Field::Assignee => task.assignee = Some(value.into()),
        Field::Goal => task.goal_id = Some(value.into()),
        Field::Created => task.created_at = value.into(),
        Field::Updated => task.updated_at = value.into(),
    }
}

async fn accepted_keys(adapter: Adapter) {
    let fixture = Fixture::new(adapter);
    // UTF-8 byte limits, including a multibyte value at exactly 65,536 bytes.
    for (index, field) in [Field::Id, Field::Assignee, Field::Goal]
        .into_iter()
        .enumerate()
    {
        for (suffix, value) in [("empty", String::new()), ("max", "é".repeat(32_768))] {
            let mut entity = task(&format!("valid-{index}-{suffix}"));
            set(&mut entity, field, &value);
            let id = entity.id().to_owned();
            let expected = entity.clone();
            let saved = fixture.create(entity).await;
            assert_eq!(saved.entity, expected);
            assert_eq!(fixture.get(EntityKind::Task, &id).await, Some(saved));
        }
    }
}

async fn accepted_times(adapter: Adapter) {
    let fixture = Fixture::new(adapter);
    for (index, time) in ["0000-01-01T00:00:00.000Z", "9999-12-30T22:00:00.999Z"]
        .into_iter()
        .enumerate()
    {
        let mut entity = task(&format!("time-{index}"));
        set(&mut entity, Field::Created, time);
        set(&mut entity, Field::Updated, time);
        let expected = entity.clone();
        let saved = fixture.create(entity).await;
        assert_eq!(saved.entity, expected);
        assert_eq!(
            fixture
                .get(EntityKind::Task, &format!("time-{index}"))
                .await,
            Some(saved)
        );
    }
}

async fn large_non_key_text(adapter: Adapter) {
    let fixture = Fixture::new(adapter);
    let mut entity = task("large-text");
    if let Entity::Task(task) = &mut entity {
        task.title = "é".repeat(32_769);
        task.description = "x".repeat(65_537);
    }
    let saved = fixture.create(entity.clone()).await;
    assert_eq!(saved.entity, entity);
    assert_eq!(
        fixture.get(EntityKind::Task, "large-text").await,
        Some(saved)
    );
}

async fn refused_field(adapter: Adapter, field: Field, value: String) {
    let fixture = Fixture::new(adapter);
    let first = fixture.create(task("first")).await;
    let mut invalid = task("refused");
    set(&mut invalid, field, &value);
    let refused_id = invalid.id().to_owned();
    let changed = invalid.clone();
    let baseline = first.clone();
    let create_error = fixture
        .storage
        .transaction(owner(), Lane::Entities, move |unit| {
            let error = unit.create(invalid).unwrap_err();
            assert!(unit.get(EntityKind::Task, &refused_id)?.is_none());
            assert_eq!(unit.get(EntityKind::Task, "first")?, Some(baseline));
            let second = unit.create(task("second"))?;
            Ok((error, second))
        })
        .await
        .unwrap();
    assert_eq!(
        create_error.1.position,
        first.position + 1,
        "refusal consumes no position"
    );
    // Check state independently of the public category, including on an unwired adapter.
    assert_eq!(
        fixture.get(EntityKind::Task, "first").await,
        Some(first.clone())
    );
    if !matches!(field, Field::Id) {
        let mut changed = changed;
        set(&mut changed, Field::Id, "first");
        let revision = first.revision;
        let error = fixture
            .storage
            .transaction(owner(), Lane::Entities, move |unit| {
                Ok(unit.update(changed, revision).unwrap_err())
            })
            .await
            .unwrap();
        assert_eq!(fixture.get(EntityKind::Task, "first").await, Some(first));
        refusal(error);
    }
    refusal(create_error.0);
}

async fn rollback(adapter: Adapter) {
    let fixture = Fixture::new(adapter);
    let first = fixture.create(task("first")).await;
    let revision = first.revision;
    let error = fixture
        .storage
        .transaction(owner(), Lane::Entities, move |unit| {
            let mut edited = task("first");
            if let Entity::Task(task) = &mut edited {
                task.title = "provisional".into();
            }
            unit.update(edited, revision)?;
            unit.create(task("provisional"))?;
            let mut invalid = task("invalid");
            set(&mut invalid, Field::Updated, "private-invalid-time");
            unit.create(invalid)
        })
        .await
        .unwrap_err();
    assert_eq!(
        fixture.get(EntityKind::Task, "first").await,
        Some(first.clone())
    );
    assert!(fixture.get(EntityKind::Task, "provisional").await.is_none());
    let next = fixture.create(task("next")).await;
    assert_eq!(next.position, first.position + 1);
    refusal(error);
}

async fn context_provenance(adapter: Adapter) {
    let fixture = Fixture::new(adapter);
    let mut caller = owner();
    caller.principal = Principal::Owner {
        identity: "é".repeat(32_768),
    };
    fixture
        .storage
        .transaction(caller.clone(), Lane::Entities, |unit| {
            unit.create(context())
        })
        .await
        .unwrap();
    caller.principal = Principal::Owner {
        identity: "é".repeat(32_768) + "x",
    };
    let before = fixture.get(EntityKind::Context, "context").await.unwrap();
    let error = fixture
        .storage
        .transaction(caller.clone(), Lane::Entities, |unit| {
            let Entity::Context(mut new) = context() else {
                unreachable!()
            };
            new.key = "refused".into();
            unit.create(Entity::Context(new))
        })
        .await
        .unwrap_err();
    assert!(fixture.get(EntityKind::Context, "refused").await.is_none());
    assert_eq!(
        fixture.get(EntityKind::Context, "context").await,
        Some(before.clone())
    );
    // Existing provenance is preserved: the current principal is not new provenance on update.
    let revision = before.revision;
    let updated = fixture
        .storage
        .transaction(caller, Lane::Entities, move |unit| {
            let Entity::Context(mut entry) = before.entity else {
                unreachable!()
            };
            entry.value = "updated".into();
            unit.update(Entity::Context(entry), revision)
        })
        .await
        .unwrap();
    assert_eq!(
        fixture.get(EntityKind::Context, "context").await,
        Some(updated)
    );
    refusal(error);
}

async fn reopen_after_refusal(adapter: Adapter) {
    let mut fixture = Fixture::new(adapter);
    let before = fixture.create(task("first")).await;
    let error = fixture
        .storage
        .transaction(owner(), Lane::Entities, |unit| {
            let mut invalid = task("refused");
            set(&mut invalid, Field::Goal, &"x".repeat(65_537));
            Ok(unit.create(invalid).unwrap_err())
        })
        .await
        .unwrap();
    fixture.reopen().await;
    assert_eq!(
        fixture.get(EntityKind::Task, "first").await,
        Some(before.clone())
    );
    assert!(fixture.get(EntityKind::Task, "refused").await.is_none());
    let next = fixture.create(task("next")).await;
    assert_eq!(next.position, before.position + 1);
    refusal(error);
}

macro_rules! matrix {
    ($module:ident, $adapter:expr) => {
        mod $module {
            use super::*;
            #[tokio::test]
            async fn empty_and_maximum_utf8_keys_round_trip() {
                accepted_keys($adapter).await;
            }
            #[tokio::test]
            async fn canonical_time_extrema_round_trip() {
                accepted_times($adapter).await;
            }
            #[tokio::test]
            async fn oversized_id_is_refused_without_mutation() {
                refused_field($adapter, Field::Id, "é".repeat(32_768) + "x").await;
            }
            #[tokio::test]
            async fn oversized_assignee_is_refused_without_mutation() {
                refused_field($adapter, Field::Assignee, "é".repeat(32_768) + "x").await;
            }
            #[tokio::test]
            async fn oversized_goal_is_refused_without_mutation() {
                refused_field($adapter, Field::Goal, "é".repeat(32_768) + "x").await;
            }
            #[tokio::test]
            async fn second_precision_created_time_is_refused() {
                refused_field($adapter, Field::Created, "2026-01-01T00:00:00Z".into()).await;
            }
            #[tokio::test]
            async fn offset_updated_time_is_refused() {
                refused_field(
                    $adapter,
                    Field::Updated,
                    "2026-01-01T00:00:00.000+00:00".into(),
                )
                .await;
            }
            #[tokio::test]
            async fn impossible_date_is_refused() {
                refused_field($adapter, Field::Created, "2026-02-30T00:00:00.000Z".into()).await;
            }
            #[tokio::test]
            async fn time_after_storage_maximum_is_refused() {
                refused_field($adapter, Field::Updated, "9999-12-30T22:00:01.000Z".into()).await;
            }
            #[tokio::test]
            async fn empty_time_is_refused() {
                refused_field($adapter, Field::Created, String::new()).await;
            }
            #[tokio::test]
            async fn invalid_month_is_refused() {
                refused_field($adapter, Field::Updated, "2026-13-01T00:00:00.000Z".into()).await;
            }
            #[tokio::test]
            async fn oversized_created_time_is_refused() {
                refused_field($adapter, Field::Created, "x".repeat(65_537)).await;
            }
            #[tokio::test]
            async fn oversized_updated_time_is_refused() {
                refused_field($adapter, Field::Updated, "x".repeat(65_537)).await;
            }
            #[tokio::test]
            async fn non_key_text_over_inline_limit_round_trips() {
                large_non_key_text($adapter).await;
            }
            #[tokio::test]
            async fn expanded_year_is_refused() {
                refused_field(
                    $adapter,
                    Field::Updated,
                    "+10000-01-01T00:00:00.000Z".into(),
                )
                .await;
            }
            #[tokio::test]
            async fn returned_refusal_rolls_back_rows_revisions_and_positions() {
                rollback($adapter).await;
            }
            #[tokio::test]
            async fn context_creation_bounds_provenance_and_update_preserves_it() {
                context_provenance($adapter).await;
            }
        }
    };
}
matrix!(memory, Adapter::Memory);
matrix!(file, Adapter::File);
matrix!(sqlite, Adapter::Sqlite);

#[tokio::test]
async fn file_refusal_preserves_durable_rows_and_position_after_reopen() {
    reopen_after_refusal(Adapter::File).await;
}
#[tokio::test]
async fn sqlite_refusal_preserves_durable_rows_and_position_after_reopen() {
    reopen_after_refusal(Adapter::Sqlite).await;
}

#[tokio::test]
async fn file_io_failure_is_backend_and_preserves_the_canary() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("not-a-directory");
    std::fs::write(&path, b"synthetic canary").unwrap();
    let storage = FileStorage::new(&path);
    let error = storage.read(owner()).await.err().expect("not a directory");
    assert!(matches!(
        error,
        StoreError::Backend(orch_core::Error::Io { .. })
    ));
    assert_eq!(std::fs::read(path).unwrap(), b"synthetic canary");
}

#[tokio::test]
async fn file_stored_damage_is_a_decode_error_and_is_not_overwritten() {
    let fixture = Fixture::new(Adapter::File);
    fixture.create(task("first")).await;
    let path = fixture.root.path().join("file/p-70/state.json");
    std::fs::write(&path, b"{synthetic broken json").unwrap();
    let error = fixture
        .storage
        .read(owner())
        .await
        .err()
        .expect("damaged document");
    assert!(matches!(
        error,
        StoreError::Backend(orch_core::Error::Serde { .. })
    ));
    let error = fixture
        .storage
        .transaction(owner(), Lane::Entities, |unit| unit.create(task("later")))
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        StoreError::Backend(orch_core::Error::Serde { .. })
    ));
    assert_eq!(std::fs::read(path).unwrap(), b"{synthetic broken json");
}

#[tokio::test]
async fn sqlite_statement_fault_is_backend_without_latching_and_can_be_retried() {
    let fixture = Fixture::new(Adapter::Sqlite);
    let data = fixture.data.as_ref().unwrap();
    let sql = open_connection(data, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    sql.execute_batch("CREATE TRIGGER refuse_insert BEFORE INSERT ON tasks BEGIN SELECT RAISE(ABORT, 'synthetic backend fault'); END").unwrap();
    let error = fixture
        .storage
        .transaction(owner(), Lane::Entities, |unit| unit.create(task("fault")))
        .await
        .unwrap_err();
    assert!(matches!(error, StoreError::Backend(_)));
    assert_eq!(error.to_string(), "SQLite storage operation failed");
    assert!(fixture.get(EntityKind::Task, "fault").await.is_none());
    sql.execute_batch("DROP TRIGGER refuse_insert").unwrap();
    fixture.create(task("fault")).await;
}

#[tokio::test]
async fn sqlite_stored_damage_is_fatal_corruption_instead_of_input_refusal() {
    let root = tempfile::tempdir().unwrap();
    let data = Arc::new(
        DataDir::acquire(
            &root.path().join("data"),
            &Holder {
                kind: LockKind::Daemon,
                pid: std::process::id(),
                started_at: "synthetic".into(),
                binary_version: "test".into(),
                executable: "/synthetic/orch".into(),
            },
        )
        .unwrap(),
    );
    let storage = SqliteStorage::open(data.clone(), StorageOptions::default()).unwrap();
    storage
        .transaction(owner(), Lane::Entities, |unit| unit.create(task("first")))
        .await
        .unwrap();
    open_connection(&data, ConnectionRole::Writer, Synchronous::Normal)
        .unwrap()
        .execute_batch("DELETE FROM task_details WHERE project_id='p' AND task_id='first'")
        .unwrap();
    let error = storage
        .read(owner())
        .await
        .unwrap()
        .get(EntityKind::Task, "first")
        .await
        .unwrap_err();
    assert!(matches!(error, StoreError::Backend(_)));
    assert_eq!(error.to_string(), "storage_unavailable");
    assert!(
        storage
            .failure_state()
            .failure()
            .unwrap()
            .causes
            .contains(&(Role::Operation, FatalCause::LogicalCorruption))
    );
    storage.close().await.unwrap();
}
