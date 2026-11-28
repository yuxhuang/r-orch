//! Composed regressions for the public SQLite `Storage` (issue 286; R128, R140, R141).
//!
//! Each test drives the real owned adapter through `Storage::write` and `read`:
//! the writer thread, the executor's commit gate, the entity router, the
//! context/config/template/goal helper and the shared failure state, with no
//! helper built by hand. A callback has no raw connection, so SQLite is made to
//! end the transaction under it the way a full disk or an I/O error would: a
//! trigger, installed through a second connection of the same data directory,
//! raises `ROLLBACK`. Damage for the fatal cases is written the same way.
//!
//! What each leaf proves alone stays with its leaf: the two native guards in
//! `agent_team_run` (poison with a transaction open, a fresh helper on an ended
//! transaction) and the typed-cause cases of `sqlite_writer_policy`. The tests
//! here prove the composition: returned cleanup failures poison the whole unit,
//! while Task, Agent/Team/Run and native cctg cleanup failures unwind and retire the writer. Neither path
//! commits the failed request, including when the callback intended to return Ok.
use orch_core::storage::ContextEntry;
use orch_core::store::{
    Caller, ConfigEntry, Entity, EntityKind, Filter, Lane, PageRequest, Principal, Record,
    Revision, Storage, StorageExt, StoreError, TemplateEntry, UnitOfWork,
};
use orch_infra::data_dir::{DataDir, Holder, LockKind};
use orch_infra::sqlite::{
    ConnectionRole, FailureSource, FatalCause, Role, SqliteStorage, StorageOptions, Synchronous,
    open_connection,
};
use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// The only text a caller ever sees for a backend failure that is not fatal.
const FAILED: &str = "SQLite storage operation failed";
/// The answer of a writer or reader whose shared state is latched.
const UNAVAILABLE: &str = "storage_unavailable";

fn owner() -> Caller {
    Caller {
        project: "p".into(),
        principal: Principal::Owner {
            identity: "synthetic".into(),
        },
    }
}

struct Env {
    _root: tempfile::TempDir,
    data: Arc<DataDir>,
    storage: SqliteStorage,
}

impl Env {
    fn new() -> Self {
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
        Self {
            _root: root,
            data,
            storage,
        }
    }

    /// A second connection of the same database. It is how a test makes SQLite
    /// misbehave under a callback, and how it counts rows without the adapter.
    fn sql(&self, script: &str) {
        open_connection(&self.data, ConnectionRole::Writer, Synchronous::Normal)
            .unwrap()
            .execute_batch(script)
            .unwrap();
    }

    fn count(&self, table: &str) -> i64 {
        open_connection(&self.data, ConnectionRole::Reader, Synchronous::Normal)
            .unwrap()
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    }

    async fn get(&self, kind: EntityKind, id: &str) -> Option<Record> {
        let snapshot = self.storage.read(owner()).await.unwrap();
        snapshot.get(kind, id).await.unwrap()
    }

    /// One committed transaction that must succeed.
    async fn commit<T: Send + 'static>(
        &self,
        work: impl FnOnce(&mut dyn UnitOfWork) -> orch_core::store::Result<T> + Send + 'static,
    ) -> T {
        self.storage
            .transaction(owner(), Lane::Entities, work)
            .await
            .expect("the transaction commits")
    }

    /// A transaction whose callback returns `Ok`, whatever it caught on the way.
    async fn try_commit(
        &self,
        work: impl FnOnce(&mut dyn UnitOfWork) + Send + 'static,
    ) -> orch_core::store::Result<()> {
        self.storage
            .write(
                owner(),
                Lane::Entities,
                Box::new(move |unit| {
                    work(unit);
                    Ok(())
                }),
            )
            .await
    }

    async fn close(self) {
        self.storage.close().await.unwrap();
    }
}

/// The text of a refusal that is not a typed request answer.
fn backend_text(error: &StoreError) -> String {
    match error {
        StoreError::Backend(inner) => inner.to_string(),
        other => panic!("expected a backend failure, got {other:?}"),
    }
}

fn agent(id: &str, name: &str) -> Entity {
    Entity::Agent(Box::new(
        serde_json::from_value(json!({
            "id":id,"name":name,"adapter":"shell","config":{},"status":"idle",
            "stats":{"tasks_completed":0,"tasks_failed":0,"total_runs":0,"total_runtime_ms":0}
        }))
        .unwrap(),
    ))
}
fn team(id: &str) -> Entity {
    Entity::Team(Box::new(
        serde_json::from_value(json!({
            "id":id,"name":id,"status":"active","members":[],"task_pool":[],
            "lead_agent_id":"a","created_at":"opaque","updated_at":"opaque",
            "config":{"auto_claim":false}
        }))
        .unwrap(),
    ))
}
fn run(id: &str) -> Entity {
    Entity::Run(Box::new(
        serde_json::from_value(json!({
            "id":id,"task_id":id,"agent_id":"a","attempt":1,"status":"running",
            "started_at":"opaque","workspace_path":"","prompt":"p"
        }))
        .unwrap(),
    ))
}
fn task(id: &str) -> Entity {
    Entity::Task(Box::new(
        serde_json::from_value(json!({
            "id":id,"title":id,"description":"","status":"todo","priority":2,
            "created_at":"2026-11-20T10:00:00.000Z","updated_at":"2026-11-20T10:00:00.000Z",
            "attempts":0,"max_attempts":3
        }))
        .unwrap(),
    ))
}
fn message(id: &str) -> Entity {
    Entity::Message(Box::new(
        serde_json::from_value(json!({
            "id":id,"channel":"broadcast","from_agent_id":"a","to_agent_id":null,
            "subject":id,"body":"body","created_at":"opaque","status":"pending"
        }))
        .unwrap(),
    ))
}
fn goal(id: &str) -> Entity {
    Entity::Goal(Box::new(
        serde_json::from_value(json!({
            "id":id,"title":id,"description":"","status":"active",
            "created_at":"2026-11-22T00:00:00.123Z"
        }))
        .unwrap(),
    ))
}
fn context(key: &str) -> Entity {
    Entity::Context(ContextEntry {
        key: key.into(),
        value: "v".into(),
        created_at: "2026-11-22T00:00:00.123Z".into(),
        updated_at: "2026-11-22T00:00:01.000Z".into(),
        ttl_ms: None,
        expires_at: None,
    })
}
fn config(key: &str) -> Entity {
    Entity::Config(ConfigEntry {
        key: key.into(),
        value: "v".into(),
    })
}
fn template(name: &str) -> Entity {
    Entity::Template(TemplateEntry {
        name: name.into(),
        body: "b".into(),
    })
}

/// One entity of every family, each with an id the triggers below never match.
fn every_family(suffix: &str) -> Vec<Entity> {
    let id = format!("after-{suffix}");
    vec![
        task(&id),
        goal(&id),
        agent(&id, &id),
        team(&id),
        run(&id),
        message(&id),
        context(&id),
        config(&id),
        template(&id),
    ]
}

/// Families whose helper can lose its savepoint: each is a different owner of the
/// poison flag inside the one `UnitOfWork`. `trigger` ends the transaction when
/// the entity named `doomed` is inserted.
struct Failing {
    name: &'static str,
    trigger: &'static str,
    entity: fn() -> Entity,
    retires_writer: bool,
}
fn failing_families() -> [Failing; 3] {
    [
        // Agent, Team and Run share the native helper behind the entity router.
        Failing {
            name: "agent (native router helper)",
            trigger: "CREATE TRIGGER end_it BEFORE INSERT ON agents WHEN NEW.id='doomed' \
                      BEGIN SELECT RAISE(ROLLBACK,'synthetic SQLite rollback'); END",
            entity: || agent("doomed", "doomed"),
            retires_writer: true,
        },
        // The task writer unwinds on failed savepoint cleanup (#477).
        Failing {
            name: "task (task writer)",
            trigger: "CREATE TRIGGER end_it BEFORE INSERT ON tasks WHEN NEW.id='doomed' \
                      BEGIN SELECT RAISE(ROLLBACK,'synthetic SQLite rollback'); END",
            entity: || task("doomed"),
            retires_writer: true,
        },
        // Context, config, template and goal are a separate helper in the unit.
        Failing {
            name: "config (context/config/template/goal helper)",
            trigger: "CREATE TRIGGER end_it BEFORE INSERT ON project_config WHEN NEW.key='doomed' \
                      BEGIN SELECT RAISE(ROLLBACK,'synthetic SQLite rollback'); END",
            entity: || config("doomed"),
            retires_writer: true,
        },
    ]
}

/// R128: SQLite ended the transaction under the unit, so its savepoint cannot be
/// rolled back and the unit may never commit. Task, Agent/Team/Run and cctg leaves
/// unwind and retire the writer (#477, #468 and #462). No path commits partial work;
/// a reopened writer is usable.
#[tokio::test]
async fn a_caught_rollback_failure_refuses_the_rest_of_the_unit_and_commits_nothing() {
    for failing in failing_families() {
        let mut env = Env::new();
        env.sql(failing.trigger);
        let keep = env
            .commit(|unit| unit.create(agent("keep", "original")))
            .await;
        assert_eq!(keep.revision, Revision::ONE);

        let calls = Arc::new(AtomicUsize::new(0));
        let seen = calls.clone();
        let failing_entity = (failing.entity)();
        let outcome = env
            .try_commit(move |unit| {
                // Work the unit completed before SQLite ended its transaction.
                unit.update(agent("keep", "renamed"), keep.revision)
                    .expect("an update before the failure");
                unit.create(team("before")).expect("a create before");
                let error = unit.create(failing_entity).unwrap_err();
                assert_eq!(backend_text(&error), FAILED);
                // The callback swallows that and goes on. Every family refuses,
                // reads and lists included, without touching a row.
                for entity in every_family("late") {
                    let kind = entity.kind();
                    let error = unit.create(entity).unwrap_err();
                    assert_eq!(backend_text(&error), FAILED, "create {kind:?}");
                    let error = unit.get(kind, "keep").unwrap_err();
                    assert_eq!(backend_text(&error), FAILED, "get {kind:?}");
                    let error = unit.delete(kind, "keep", Revision::ONE).unwrap_err();
                    assert_eq!(backend_text(&error), FAILED, "delete {kind:?}");
                    let error = unit.move_to_end(kind, "keep", Revision::ONE).unwrap_err();
                    assert_eq!(backend_text(&error), FAILED, "move {kind:?}");
                }
                let error = unit
                    .update(agent("keep", "again"), keep.revision)
                    .unwrap_err();
                assert_eq!(backend_text(&error), FAILED);
                let error = unit
                    .list(&Filter::Agents, &PageRequest::default())
                    .unwrap_err();
                assert_eq!(backend_text(&error), FAILED);
                seen.fetch_add(1, Ordering::SeqCst);
            })
            .await;
        assert_eq!(
            calls.load(Ordering::SeqCst),
            usize::from(!failing.retires_writer),
            "{}",
            failing.name
        );
        let error = outcome.expect_err(failing.name);
        if failing.retires_writer {
            assert!(backend_text(&error).contains("SQLite writer is closed"));
        } else {
            assert_eq!(backend_text(&error), FAILED, "{}", failing.name);
        }
        assert!(
            env.storage.failure_state().failure().is_none(),
            "{}: a rollback failure with ordinary causes does not latch",
            failing.name
        );

        // Nothing of the request is durable: not the early work, not a late
        // statement that would have run on its own after the transaction ended.
        let kept = env.get(EntityKind::Agent, "keep").await.unwrap();
        assert_eq!(kept.entity, agent("keep", "original"), "{}", failing.name);
        assert_eq!(kept.revision, Revision::ONE, "{}", failing.name);
        for table in [
            "agents",
            "teams",
            "runs",
            "tasks",
            "goals",
            "project_config",
            "templates",
        ] {
            let expected = i64::from(table == "agents");
            assert_eq!(env.count(table), expected, "{}: {table}", failing.name);
        }

        env.sql("DROP TRIGGER end_it");
        if failing.retires_writer {
            let later = env
                .storage
                .transaction(owner(), Lane::Entities, |unit| unit.create(team("refused")))
                .await
                .unwrap_err();
            assert!(backend_text(&later).contains("SQLite writer is closed"));
            assert!(env.storage.close().await.is_err());
            env.storage = SqliteStorage::open(env.data.clone(), StorageOptions::default()).unwrap();
        }
        // A returned abort leaves the worker usable; typed cleanup requires a
        // reopened worker before a new transaction can commit.
        env.commit(|unit| unit.create(team("fresh"))).await;
        assert!(env.get(EntityKind::Team, "fresh").await.is_some());
        assert!(env.get(EntityKind::Team, "before").await.is_none());
        assert!(env.storage.failure_state().failure().is_none());
        env.close().await;
    }
}

/// The opposite, passing control: an ordinary refusal that SQLite rolled back to
/// the savepoint (the transaction stays open) leaves the unit usable, and
/// unrelated work commits.
#[tokio::test]
async fn an_ordinary_refusal_rolled_back_to_its_savepoint_lets_the_unit_commit() {
    for failing in failing_families() {
        let env = Env::new();
        env.sql(&failing.trigger.replace("ROLLBACK", "ABORT"));
        let failing_entity = (failing.entity)();
        env.commit(move |unit| {
            unit.create(team("before"))?;
            let error = unit.create(failing_entity).unwrap_err();
            assert_eq!(backend_text(&error), FAILED);
            unit.create(team("after"))?;
            Ok(())
        })
        .await;
        // Both teams are durable, the refused entity is not, and nothing latched.
        assert!(env.get(EntityKind::Team, "before").await.is_some());
        assert!(env.get(EntityKind::Team, "after").await.is_some());
        assert_eq!(env.count("teams"), 2, "{}", failing.name);
        assert!(env.storage.failure_state().failure().is_none());
        env.close().await;
    }
}

/// R140/R141: proven corruption of a live row, even when the callback catches the
/// error, fails the writer. Nothing of the request is durable, the request answers
/// with the latch, and every later request is refused.
#[tokio::test]
async fn caught_live_corruption_latches_the_writer_and_prevents_the_outer_commit() {
    let env = Env::new();
    env.commit(|unit| unit.create(agent("a", "original"))).await;
    // A value the schema accepts and the reader proves wrong.
    env.sql("UPDATE agents SET status='drift'");

    let outcome = env
        .try_commit(|unit| {
            unit.create(team("during"))
                .expect("created before the read");
            let error = unit.get(EntityKind::Agent, "a").unwrap_err();
            assert_eq!(backend_text(&error), UNAVAILABLE);
            // Caught. The shared latch now refuses every statement of this unit.
            let error = unit.create(team("after")).unwrap_err();
            assert_eq!(backend_text(&error), UNAVAILABLE);
            let error = unit.get(EntityKind::Team, "during").unwrap_err();
            assert_eq!(backend_text(&error), UNAVAILABLE);
        })
        .await;
    assert_eq!(backend_text(&outcome.unwrap_err()), UNAVAILABLE);
    assert_eq!(env.count("teams"), 0, "nothing after the latch is durable");

    let failure = env.storage.failure_state().failure().expect("latched");
    assert_eq!(failure.source, FailureSource::Writer);
    assert_eq!(
        failure.causes,
        [(Role::Operation, FatalCause::LogicalCorruption)]
    );
    // Signed #389 B: a prior latch refuses mutations while a new snapshot
    // may still serve reads that observe no fresh damage.
    let later = env.try_commit(|_| ()).await;
    assert_eq!(backend_text(&later.unwrap_err()), UNAVAILABLE);
    let snapshot = env.storage.read(owner()).await.unwrap();
    assert!(
        snapshot
            .get(EntityKind::Config, "healthy-absent")
            .await
            .unwrap()
            .is_none()
    );
    drop(snapshot);
    env.close().await;
}

/// R140: the operation's cause and the rollback's cause stay apart in what the
/// failure state keeps, and each decides the outcome by its own class. An ordinary
/// operation cause with a rollback that failed because SQLite ended the transaction
/// retires the native writer without latching. A fatal operation cause latches
/// with exactly that cause under the operation role. (A fatal cause on the rollback side needs
/// `SQLITE_CORRUPT` from a savepoint rollback, which no callback can provoke; the
/// synthetic cases of `sqlite_writer_policy` keep it.)
#[tokio::test]
async fn the_operation_cause_and_the_rollback_cause_keep_their_own_class() {
    let mut env = Env::new();
    env.commit(|unit| unit.create(agent("a", "original"))).await;
    env.sql(
        "CREATE TRIGGER end_it BEFORE INSERT ON teams WHEN NEW.id='doomed' \
         BEGIN SELECT RAISE(ROLLBACK,'synthetic SQLite rollback'); END",
    );

    // Ordinary operation and cleanup causes retire this native writer without
    // latching. The typed unwind skips the callback's returned-error handling.
    let outcome = env
        .try_commit(|unit| {
            let error = unit.create(team("doomed")).unwrap_err();
            assert_eq!(backend_text(&error), FAILED);
        })
        .await;
    assert!(backend_text(&outcome.unwrap_err()).contains("SQLite writer is closed"));
    assert!(env.storage.failure_state().failure().is_none());
    assert_eq!(env.count("teams"), 0);
    assert!(env.storage.close().await.is_err());
    env.storage = SqliteStorage::open(env.data.clone(), StorageOptions::default()).unwrap();

    // A fatal operation cause with the savepoint intact: the read runs, proves
    // the stored agent corrupt and the writer latches that cause as the
    // operation's.
    env.sql("DROP TRIGGER end_it; UPDATE agents SET status='drift'");
    let outcome = env
        .try_commit(|unit| {
            let error = unit.get(EntityKind::Agent, "a").unwrap_err();
            assert_eq!(backend_text(&error), UNAVAILABLE);
        })
        .await;
    assert_eq!(backend_text(&outcome.unwrap_err()), UNAVAILABLE);
    let failure = env.storage.failure_state().failure().expect("latched");
    assert_eq!(
        failure.causes,
        [(Role::Operation, FatalCause::LogicalCorruption)]
    );
    env.close().await;
}

// ---------------------------------------------------------------------------
// Issue 127 (R140; PR 202 review I2): a statement that really fails in each
// adapter, through the public `Storage`, reaches the shared failure state with
// its SQLite code. The database is damaged on disk with the adapter closed, so
// the first statement that reads the table answers SQLITE_CORRUPT.
// ---------------------------------------------------------------------------

use orch_core::store::events::SequencedEvent;
use orch_core::store::leases::{ClaimOutcome, NewLease};
use orch_core::store::request_keys::RequestScope;

fn sequenced(seq: u64) -> SequencedEvent {
    SequencedEvent {
        seq,
        event: orch_domain::RunEvent {
            timestamp: "2026-01-01T00:00:00Z".into(),
            event_type: orch_domain::RunEventType::AgentOutput,
            data: json!({ "seq": seq }),
        },
    }
}

fn key_hash() -> String {
    "a".repeat(64)
}

fn key_scope(key: &str) -> RequestScope {
    RequestScope::of(&owner(), key).unwrap()
}

/// Overwrite the first byte of the root page of `table`: not a valid b-tree page.
fn damage(data: &DataDir, table: &str) {
    use std::io::{Seek, SeekFrom, Write};
    let conn = open_connection(data, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
        .unwrap();
    let root: i64 = conn
        .query_row(
            "SELECT rootpage FROM sqlite_master WHERE name=?1",
            [table],
            |r| r.get(0),
        )
        .unwrap();
    let size: i64 = conn
        .query_row("PRAGMA page_size", [], |r| r.get(0))
        .unwrap();
    let path = conn.path().unwrap().to_owned();
    drop(conn);
    let mut file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    file.seek(SeekFrom::Start(((root - 1) * size) as u64))
        .unwrap();
    file.write_all(&[0xFF]).unwrap();
    file.sync_all().unwrap();
}

/// A storage over a database that holds one lease, two events, a gap, a request
/// key and a goal, with `table` damaged and the adapter reopened over it.
async fn damaged(table: &str) -> Env {
    let env = Env::new();
    env.commit(|unit| {
        let ClaimOutcome::Granted(lease) = unit.claim_lease(
            &NewLease {
                task: "t".into(),
                agent: "a".into(),
                run: "r".into(),
            },
            0,
        )?
        else {
            panic!("the seed lease is granted")
        };
        unit.append_events(
            "r",
            &lease.lineage,
            lease.epoch,
            &[sequenced(1), sequenced(2)],
        )?;
        unit.record_event_gap("r", 2, 1)?;
        unit.store_request_key(
            &key_scope("0123456789abcdef"),
            "task.update",
            &key_hash(),
            "r",
            0,
        )?;
        unit.create(goal("seed"))?;
        Ok(())
    })
    .await;
    let Env {
        _root,
        data,
        storage,
    } = env;
    storage.close().await.unwrap();
    damage(&data, table);
    let storage = SqliteStorage::open(data.clone(), StorageOptions::default()).unwrap();
    Env {
        _root,
        data,
        storage,
    }
}

type WriteOp = fn(&mut dyn UnitOfWork) -> orch_core::store::Result<()>;

#[tokio::test]
async fn a_real_corrupt_statement_of_each_adapter_latches_the_public_writer_with_its_cause() {
    let ops: [(&str, &str, WriteOp); 8] = [
        ("request key check", "request_keys", |u| {
            u.check_request_key(
                &key_scope("fedcba9876543210"),
                "task.update",
                &key_hash(),
                0,
            )
            .map(drop)
        }),
        ("request key store", "request_keys", |u| {
            u.store_request_key(
                &key_scope("fedcba9876543210"),
                "task.update",
                &key_hash(),
                "x",
                0,
            )
        }),
        ("lease claim", "leases", |u| {
            u.claim_lease(
                &NewLease {
                    task: "t2".into(),
                    agent: "a2".into(),
                    run: "r2".into(),
                },
                0,
            )
            .map(drop)
        }),
        ("lease get", "leases", |u| u.get_lease("t").map(drop)),
        ("event append", "run_events", |u| {
            let lease = u.get_lease("t")?.expect("the seed lease");
            u.append_events("r", &lease.lineage, lease.epoch, &[sequenced(3)])
                .map(drop)
        }),
        ("event prune", "run_events", |u| {
            u.prune_events("r", 10).map(drop)
        }),
        ("event gap", "run_event_gaps", |u| {
            u.record_event_gap("r", 3, 1)
        }),
        ("goal create", "goals", |u| {
            u.create(goal("fresh")).map(drop)
        }),
    ];
    for (name, table, op) in ops {
        let env = damaged(table).await;
        let seen = Arc::new(std::sync::Mutex::new(None));
        let slot = seen.clone();
        let outcome = env
            .storage
            .write(
                owner(),
                Lane::Entities,
                Box::new(move |unit| {
                    *slot.lock().unwrap() = Some(op(unit));
                    // Caught, and the callback goes on to succeed.
                    Ok(())
                }),
            )
            .await;
        let caught = seen.lock().unwrap().take().unwrap();
        assert_eq!(
            backend_text(&caught.expect_err(name)),
            UNAVAILABLE,
            "{name}: the first fatal observation answers unavailable"
        );
        assert_eq!(backend_text(&outcome.unwrap_err()), UNAVAILABLE, "{name}");
        let failure = env.storage.failure_state().failure().expect(name);
        assert_eq!(failure.source, FailureSource::Writer, "{name}");
        assert_eq!(
            failure.causes,
            [(Role::Operation, FatalCause::SqliteCorrupt)],
            "{name}"
        );
        let later = env.try_commit(|_| ()).await;
        assert_eq!(backend_text(&later.unwrap_err()), UNAVAILABLE, "{name}");
        // Issue 389: a new healthy read is allowed after the incident.
        let healthy = env.storage.read(owner()).await.unwrap();
        assert!(
            healthy
                .get(EntityKind::Agent, "absent")
                .await
                .unwrap()
                .is_none()
        );
        drop(healthy);
        env.close().await;
    }
}

#[tokio::test]
async fn a_real_corrupt_statement_of_each_pooled_read_latches_the_public_storage_as_the_readers() {
    for (name, table) in [
        ("lease", "leases"),
        ("events", "run_events"),
        ("event gaps", "run_event_gaps"),
        ("goal", "goals"),
    ] {
        let env = damaged(table).await;
        let snapshot = env.storage.read(owner()).await.unwrap();
        let error = match name {
            "lease" => snapshot.read_lease("t").await.map(drop),
            "events" => snapshot.read_events("r", 0, 10).await.map(drop),
            "event gaps" => snapshot.read_event_gaps("r").await.map(drop),
            _ => snapshot.get(EntityKind::Goal, "seed").await.map(drop),
        }
        .expect_err(name);
        assert_eq!(backend_text(&error), UNAVAILABLE, "{name}");
        drop(snapshot);
        let failure = env.storage.failure_state().failure().expect(name);
        assert_eq!(failure.source, FailureSource::Reader, "{name}");
        assert_eq!(
            failure.causes,
            [(Role::Operation, FatalCause::SqliteCorrupt)],
            "{name}"
        );
        let later = env.try_commit(|_| ()).await;
        assert_eq!(backend_text(&later.unwrap_err()), UNAVAILABLE, "{name}");
        // Issue 389: a new healthy read is allowed after the incident.
        let healthy = env.storage.read(owner()).await.unwrap();
        assert!(
            healthy
                .get(EntityKind::Agent, "absent")
                .await
                .unwrap()
                .is_none()
        );
        drop(healthy);
        env.close().await;
    }
}
