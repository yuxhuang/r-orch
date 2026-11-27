//! R142 permanent SQL/disk corpus. These tests use the private composition's
//! existing synthetic entry point; they do not register a public Storage adapter.
use super::r142_harness::NativeFormatHarness;
use super::{FailureSource, FailureState, SqliteError, migrations};
use crate::blob::{BlobHash, DiskBlobStore, MAX_BLOB_BYTES};
use crate::data_dir::{DataDir, Holder, LockKind};
use orch_core::store::{Entity, EntityKind, Record, Revision, StoreError};
use orch_domain::{Agent, Run, Team};
use rusqlite::{Connection, Transaction};
use serde_json::{Value, json};
use std::path::PathBuf;

struct Fixture {
    conn: Connection,
    disk: DiskBlobStore,
    _data: DataDir,
    temp: tempfile::TempDir,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let data = DataDir::acquire(
            &temp.path().join("data"),
            &Holder {
                kind: LockKind::Daemon,
                pid: std::process::id(),
                started_at: "synthetic".into(),
                binary_version: "test".into(),
                executable: "/synthetic/orch".into(),
            },
        )
        .unwrap();
        let disk = DiskBlobStore::open(data.root()).unwrap();
        let conn = Connection::open(temp.path().join("corpus.db")).unwrap();
        conn.execute_batch("PRAGMA journal_mode=WAL;").unwrap();
        for ddl in [
            migrations::INITIAL_SCHEMA,
            migrations::ATTENTION_FACTS,
            migrations::TASK_FORMAT,
            migrations::AGENT_TEAM_RUN,
        ] {
            conn.execute_batch(ddl).unwrap();
        }
        Self {
            conn,
            disk,
            _data: data,
            temp,
        }
    }
}
fn harness<'a, 'c>(
    tx: &'a Transaction<'c>,
    disk: &'a DiskBlobStore,
    state: &FailureState,
) -> NativeFormatHarness<'a, 'c> {
    NativeFormatHarness::new(tx, disk, "p", 123, state.clone(), FailureSource::Writer)
}
fn agent() -> Agent {
    serde_json::from_value(json!({"id":"a", "name":"same", "adapter":"codex", "config":{},
        "status":"idle", "stats":{"tasks_completed":0,"tasks_failed":0,"total_runs":0,"total_runtime_ms":0}})).unwrap()
}
fn team() -> Team {
    serde_json::from_value(json!({"id":"t","name":"same","status":"active",
        "members":[{"agent_id":"a","role":"lead","joined_at":"opaque"},
                   {"agent_id":"a","role":"member","joined_at":""}],
        "task_pool":["b","a","b"],"lead_agent_id":"a","created_at":"opaque",
        "updated_at":"2026-11-20T10:00:00.1239+02:00","config":{"auto_claim":false}}))
    .unwrap()
}
fn run() -> Run {
    serde_json::from_value(json!({"id":"r","task_id":"t","agent_id":"a","attempt":10,
        "status":"running","started_at":"opaque","workspace_path":"/synthetic","prompt":""}))
    .unwrap()
}
fn entities() -> Vec<Entity> {
    vec![
        Entity::Agent(Box::new(agent())),
        Entity::Team(Box::new(team())),
        Entity::Run(Box::new(run())),
    ]
}
fn round_trip(h: &mut NativeFormatHarness<'_, '_>, entity: Entity) -> Record {
    let record = h.create(entity.clone()).unwrap();
    assert_eq!(record.entity, entity);
    assert_eq!(record.revision, Revision::ONE);
    assert_eq!(
        h.get(entity.kind(), entity.id()).unwrap(),
        Some(record.clone())
    );
    assert_eq!(
        h.hydrate(entity.kind(), &[entity.id().into()]).unwrap(),
        *std::slice::from_ref(&record)
    );
    record
}
fn stored(error: SqliteError) {
    assert!(
        matches!(error, SqliteError::EntityCorrupt { .. }),
        "{error:?}"
    );
}
fn integer(conn: &Connection, sql: &str) -> i64 {
    conn.query_row(sql, [], |r| r.get(0)).unwrap()
}

// Detects loss of optional config, capability occurrences or three-state autonomy.
#[test]
fn an_agent_round_trips_every_field_and_the_three_autonomous_states() {
    let mut f = Fixture::new();
    let tx = f.conn.transaction().unwrap();
    let state = FailureState::new();
    let mut h = harness(&tx, &f.disk, &state);
    for (i, autonomous) in [None, Some(false), Some(true)].into_iter().enumerate() {
        let mut a = agent();
        a.id = format!("a{i}");
        a.autonomous = autonomous;
        a.role = Some(String::new());
        a.current_task = Some("task".into());
        a.capabilities = vec!["b".into(), "a".into(), "b".into()];
        a.config = serde_json::from_value(json!({"command":"cmd","model":"model","effort":"high",
            "approval_policy":"manual","max_turns":4294967295_u64,"timeout_ms":u64::MAX,
            "stall_timeout_ms":0,"env":{"Z":"","A":"v"},"system_prompt":"system",
            "workspace_mode":"shared","skills":["s","s"],"prompt_mode":"eco","role_summary":"summary"})).unwrap();
        a.last_error = Some(
            serde_json::from_value(
                json!({"message":"failure","kind":"unknown","timestamp":"opaque"}),
            )
            .unwrap(),
        );
        a.remote = Some(serde_json::from_value(json!({"alias":"peer","online":false})).unwrap());
        let expected = a.clone();
        round_trip(&mut h, Entity::Agent(Box::new(a)));
        assert_eq!(
            tx.query_row(
                "SELECT autonomous FROM agents WHERE id=?1",
                [&expected.id],
                |r| r.get::<_, Option<i64>>(0)
            )
            .unwrap(),
            autonomous.map(i64::from)
        );
        let raw: String = tx
            .query_row(
                "SELECT detail FROM entity_details WHERE entity_key=?1",
                [&expected.id],
                |r| r.get(0),
            )
            .unwrap();
        let detail: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(detail["capabilities"], json!(["b", "a", "b"]));
        assert_eq!(detail["config"]["env"], json!([["Z", ""], ["A", "v"]]));
    }
    h.ensure_committable().unwrap();
    tx.commit().unwrap();
    assert_eq!(integer(&f.conn, "SELECT count(*) FROM agents"), 3);
}

// Detects collapsing member occurrences or treating task_pool as a set.
#[test]
fn a_team_keeps_member_occurrences_and_task_pool_order_and_repeats() {
    let mut f = Fixture::new();
    let tx = f.conn.transaction().unwrap();
    let state = FailureState::new();
    let mut h = harness(&tx, &f.disk, &state);
    let mut t = team();
    t.description = Some(String::new());
    t.config.max_concurrent_tasks = Some(u32::MAX);
    t.config.message_ttl_ms = Some(u64::MAX);
    round_trip(&mut h, Entity::Team(Box::new(t.clone())));
    let rows: Vec<(i64, String, String, String)> = tx
        .prepare("SELECT ordinal,agent_id,role,joined_at_text FROM team_members ORDER BY ordinal")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(
        rows,
        [
            (0, "a".into(), "lead".into(), "opaque".into()),
            (1, "a".into(), "member".into(), "".into())
        ]
    );
    assert_eq!(integer(&tx, "SELECT count(*) FROM team_tasks"), 2);
    t.members.reverse();
    t.task_pool.reverse();
    let updated = h
        .update(Entity::Team(Box::new(t.clone())), Revision::ONE)
        .unwrap();
    assert_eq!(h.get(EntityKind::Team, "t").unwrap(), Some(updated.clone()));
    assert_eq!(updated.entity, Entity::Team(Box::new(t)));
}

// Detects tokens=None being filled with zero or total being recomputed.
#[test]
fn a_run_keeps_every_field_optional_tokens_and_an_independent_total() {
    let mut f = Fixture::new();
    let tx = f.conn.transaction().unwrap();
    let state = FailureState::new();
    let mut h = harness(&tx, &f.disk, &state);
    for (i, tokens) in [
        None,
        Some(Default::default()),
        Some(orch_domain::TokenUsage {
            input: 1,
            output: 2,
            reasoning: 3,
            total: 99,
            cache_read: 7,
            cache_write: 8,
        }),
    ]
    .into_iter()
    .enumerate()
    {
        let mut r = run();
        r.id = format!("r{i}");
        r.tokens = tokens;
        r.pid = Some(u32::MAX);
        r.finished_at = Some("".into());
        r.error = Some("error".into());
        r.error_kind = Some(orch_domain::AdapterErrorKind::Unknown);
        round_trip(&mut h, Entity::Run(Box::new(r.clone())));
        assert_eq!(
            tx.query_row("SELECT tokens_total FROM runs WHERE id=?1", [&r.id], |r| {
                r.get::<_, Option<i64>>(0)
            })
            .unwrap(),
            tokens.map(|v| v.total as i64)
        );
    }
}

// Detects a uniqueness constraint accidentally keyed by names instead of ids.
#[test]
fn equal_agent_and_team_names_on_distinct_ids_coexist() {
    let mut f = Fixture::new();
    let tx = f.conn.transaction().unwrap();
    let state = FailureState::new();
    let mut h = harness(&tx, &f.disk, &state);
    for id in ["A", "a"] {
        let mut a = agent();
        a.id = id.into();
        round_trip(&mut h, Entity::Agent(Box::new(a)));
        let mut t = team();
        t.id = id.into();
        round_trip(&mut h, Entity::Team(Box::new(t)));
    }
    assert_eq!(
        integer(&tx, "SELECT count(*) FROM agents WHERE name='same'"),
        2
    );
    assert_eq!(
        integer(&tx, "SELECT count(*) FROM teams WHERE name='same'"),
        2
    );
}

// #252: each token column independently crosses the signed boundary and MAX.
#[test]
fn u64_max_and_the_signed_neighbors_round_trip_and_project_null_above_i64_max() {
    let mut f = Fixture::new();
    let tx = f.conn.transaction().unwrap();
    let state = FailureState::new();
    let mut h = harness(&tx, &f.disk, &state);
    let fields = [
        "input",
        "output",
        "reasoning",
        "cache_read",
        "cache_write",
        "total",
    ];
    for field in fields {
        for value in [
            0,
            i64::MAX as u64 - 1,
            i64::MAX as u64,
            i64::MAX as u64 + 1,
            u64::MAX,
        ] {
            let mut r = run();
            r.id = format!("{field}-{value}");
            let mut tokens = json!({"input":1,"output":2,"reasoning":3,"cache_read":4,"cache_write":5,"total":99});
            tokens[field] = json!(value);
            r.tokens = Some(serde_json::from_value(tokens.clone()).unwrap());
            round_trip(&mut h, Entity::Run(Box::new(r.clone())));
            for other in fields {
                let n = tokens[other].as_u64().unwrap();
                let actual: Option<i64> = tx
                    .query_row(
                        &format!("SELECT tokens_{other} FROM runs WHERE id=?1"),
                        [&r.id],
                        |r| r.get(0),
                    )
                    .unwrap();
                assert_eq!(actual, i64::try_from(n).ok(), "{field}={value}, {other}");
            }
        }
    }
    for field in [
        "tasks_completed",
        "tasks_failed",
        "total_runs",
        "total_runtime_ms",
        "tokens_used",
    ] {
        for value in [
            i64::MAX as u64 - 1,
            i64::MAX as u64,
            i64::MAX as u64 + 1,
            u64::MAX,
        ] {
            let mut v = serde_json::to_value(agent()).unwrap();
            v["id"] = json!(format!("{field}-{value}"));
            v["stats"][field] = json!(value);
            let a: Agent = serde_json::from_value(v).unwrap();
            round_trip(&mut h, Entity::Agent(Box::new(a.clone())));
            let actual: Option<i64> = tx
                .query_row(
                    &format!("SELECT {field} FROM agents WHERE id=?1"),
                    [&a.id],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(actual, i64::try_from(value).ok());
        }
    }
    assert_eq!(integer(&tx, "SELECT count(*) FROM runs"), 30);
    assert_eq!(integer(&tx, "SELECT count(*) FROM agents"), 20);
}

// Detects normalizing text times or truncating submillisecond projections.
#[test]
fn opaque_offset_and_equivalent_instant_times_are_kept_and_project_only_when_exact() {
    let mut f = Fixture::new();
    let tx = f.conn.transaction().unwrap();
    let state = FailureState::new();
    let mut h = harness(&tx, &f.disk, &state);
    for (i, text, millis) in [
        (0, "opaque", None),
        (1, "2026-01-01T00:00:00Z", Some(1767225600000_i64)),
        (2, "2026-01-01T01:00:00+01:00", Some(1767225600000)),
        (3, "2026-01-01T00:00:00.0001Z", None),
        (4, "", None),
    ] {
        let mut r = run();
        r.id = format!("r{i}");
        r.started_at = text.into();
        r.finished_at = Some(text.into());
        round_trip(&mut h, Entity::Run(Box::new(r.clone())));
        let actual: (String,Option<i64>,String,Option<i64>) = tx.query_row(
            "SELECT started_at_text,started_at,json_extract(detail,'$.finished_at'),finished_at FROM runs JOIN entity_details ON entity_key=id AND family='run' WHERE id=?1",[&r.id],
            |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).unwrap();
        assert_eq!(actual, (text.into(), millis, text.into(), millis));
        let mut t = team();
        t.id = format!("t{i}");
        t.created_at = text.into();
        t.updated_at = text.into();
        t.members[0].joined_at = text.into();
        round_trip(&mut h, Entity::Team(Box::new(t.clone())));
        assert_eq!(
            tx.query_row(
                "SELECT created_at,updated_at FROM teams WHERE id=?1",
                [&t.id],
                |r| Ok((r.get::<_, Option<i64>>(0)?, r.get::<_, Option<i64>>(1)?))
            )
            .unwrap(),
            (millis, millis)
        );
        assert_eq!(
            tx.query_row(
                "SELECT joined_at FROM team_members WHERE team_id=?1 AND ordinal=0",
                [&t.id],
                |r| r.get::<_, Option<i64>>(0)
            )
            .unwrap(),
            millis
        );
    }
}

// Detects inlining even an empty prompt or publishing duplicate prompt objects.
#[test]
fn the_prompt_is_always_a_tag_and_equal_prompts_share_one_object() {
    let mut f = Fixture::new();
    let tx = f.conn.transaction().unwrap();
    let state = FailureState::new();
    let mut h = harness(&tx, &f.disk, &state);
    for (i, prompt) in ["", "same", "same"].into_iter().enumerate() {
        let mut r = run();
        r.id = format!("r{i}");
        r.prompt = prompt.into();
        round_trip(&mut h, Entity::Run(Box::new(r.clone())));
        let raw: String = tx
            .query_row(
                "SELECT detail FROM entity_details WHERE family='run' AND entity_key=?1",
                [&r.id],
                |r| r.get(0),
            )
            .unwrap();
        let value: Value = serde_json::from_str(&raw).unwrap();
        let hash = BlobHash::of(prompt.as_bytes()).to_string();
        assert_eq!(
            value["prompt"]["$blob"],
            json!({"sha256":hash,"bytes":prompt.len(),"chunks":1})
        );
        let path = f
            .temp
            .path()
            .join("data")
            .join("blobs/sha256")
            .join(&hash[..2])
            .join(&hash[2..4])
            .join(&hash);
        assert_eq!(std::fs::read(path).unwrap(), prompt.as_bytes());
        assert_eq!(tx.query_row("SELECT sha256,size,ordinal FROM entity_blob_refs WHERE entity_key=?1 AND locator='/prompt'",[&r.id],
            |r|Ok((r.get::<_,String>(0)?,r.get::<_,i64>(1)?,r.get::<_,i64>(2)?))).unwrap(),(hash,prompt.len() as i64,0));
    }
    assert_eq!(integer(&tx, "SELECT count(*) FROM blobs"), 2);
    assert_eq!(integer(&tx, "SELECT count(*) FROM entity_blob_refs"), 3);
}

fn from_payload(kind: EntityKind, value: Value) -> Entity {
    match kind {
        EntityKind::Agent => Entity::Agent(Box::new(serde_json::from_value(value).unwrap())),
        EntityKind::Team => Entity::Team(Box::new(serde_json::from_value(value).unwrap())),
        EntityKind::Run => Entity::Run(Box::new(serde_json::from_value(value).unwrap())),
        _ => unreachable!(),
    }
}
fn blob_path(root: &std::path::Path, hash: &str) -> PathBuf {
    root.join("blobs/sha256")
        .join(&hash[..2])
        .join(&hash[2..4])
        .join(hash)
}
fn refs(tx: &Transaction<'_>, locator: &str) -> Vec<(i64, String, i64)> {
    tx.prepare("SELECT ordinal,sha256,size FROM entity_blob_refs WHERE locator=?1 ORDER BY ordinal")
        .unwrap()
        .query_map([locator], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

fn text_cases() -> Vec<(EntityKind, &'static str, Value, Vec<&'static str>)> {
    let mut a = serde_json::to_value(agent()).unwrap();
    a["role"] = json!("");
    a["current_task"] = json!("");
    a["last_error"] = json!({"message":"","kind":"unknown","timestamp":""});
    a["remote"] = json!({"alias":"","online":false});
    a["capabilities"] = json!([""]);
    a["config"] = json!({"command":"","model":"","system_prompt":"","role_summary":"","skills":[""],"env":{"k":""}});
    let mut t = serde_json::to_value(team()).unwrap();
    t["description"] = json!("");
    let mut r = serde_json::to_value(run()).unwrap();
    r["finished_at"] = json!("");
    r["error"] = json!("");
    vec![
        (
            EntityKind::Agent,
            "agent",
            a,
            vec![
                "/id",
                "/name",
                "/adapter",
                "/role",
                "/current_task",
                "/last_error/message",
                "/last_error/kind",
                "/last_error/timestamp",
                "/remote/alias",
                "/capabilities/0",
                "/config/command",
                "/config/model",
                "/config/system_prompt",
                "/config/role_summary",
                "/config/skills/0",
                "/config/env/k",
            ],
        ),
        (
            EntityKind::Team,
            "team",
            t,
            vec![
                "/id",
                "/name",
                "/description",
                "/lead_agent_id",
                "/created_at",
                "/updated_at",
                "/members/0/agent_id",
                "/members/0/joined_at",
                "/task_pool/0",
            ],
        ),
        (
            EntityKind::Run,
            "run",
            r,
            vec![
                "/id",
                "/task_id",
                "/agent_id",
                "/started_at",
                "/finished_at",
                "/workspace_path",
                "/prompt",
                "/error",
            ],
        ),
    ]
}

// Detects character-count thresholds, inlined nested values/env names, missing
// aggregate spilling, splitting by characters, or collapsing repeated chunks.
#[test]
fn the_threshold_counts_utf8_bytes_for_every_text_slot_of_each_family() {
    let mut f = Fixture::new();
    let root = f.temp.path().join("data");
    let tx = f.conn.transaction().unwrap();
    let state = FailureState::new();
    let mut h = harness(&tx, &f.disk, &state);
    for (kind, family, base, slots) in text_cases() {
        for slot in slots {
            for n in [65536, 65537] {
                let text = format!("{}{}", "é".repeat(n / 2), if n % 2 == 0 { "" } else { "x" });
                assert_eq!(text.len(), n);
                let mut v = base.clone();
                *v.pointer_mut(slot).unwrap() = json!(text);
                let e = from_payload(kind, v);
                let record = round_trip(&mut h, e.clone());
                let locator = if slot == "/config/env/k" {
                    "/config/env/0/1"
                } else {
                    slot
                };
                let chunks = refs(&tx, locator);
                if n > 65536 || slot == "/prompt" {
                    assert_eq!(chunks.len(), 1, "{family} {slot} {n}");
                    assert_eq!(
                        chunks[0],
                        (0, BlobHash::of(text.as_bytes()).to_string(), n as i64)
                    );
                    assert_eq!(
                        std::fs::read(blob_path(&root, &chunks[0].1)).unwrap(),
                        text.as_bytes()
                    );
                } else {
                    assert!(chunks.is_empty(), "{family} {slot}");
                }
                if slot == "/id" {
                    let form: i64 = tx
                        .query_row(
                            "SELECT key_form FROM entity_details WHERE family=?1",
                            [family],
                            |r| r.get(0),
                        )
                        .unwrap();
                    assert_eq!(form, i64::from(n > 65536));
                }
                h.delete(kind, e.id(), record.revision).unwrap();
            }
        }
    }
    // Environment names have their own tag, rather than becoming JSON keys.
    for n in [65536, 65537] {
        let name = "k".repeat(n);
        let mut a = agent();
        a.config.env = Some([(name.clone(), "v".into())].into_iter().collect());
        let e = Entity::Agent(Box::new(a));
        let rec = round_trip(&mut h, e);
        assert_eq!(refs(&tx, "/config/env/0/0").len(), usize::from(n > 65536));
        h.delete(EntityKind::Agent, "a", rec.revision).unwrap();
    }
    // No individual field crosses the inline limit, but aggregate detail does.
    for kind in [EntityKind::Agent, EntityKind::Team, EntityKind::Run] {
        let mut value = match kind {
            EntityKind::Agent => serde_json::to_value(agent()).unwrap(),
            EntityKind::Team => serde_json::to_value(team()).unwrap(),
            _ => serde_json::to_value(run()).unwrap(),
        };
        let (left, right, family) = match kind {
            EntityKind::Agent => ("/name", "/adapter", "agent"),
            EntityKind::Team => ("/name", "/lead_agent_id", "team"),
            _ => ("/workspace_path", "/started_at", "run"),
        };
        *value.pointer_mut(left).unwrap() = json!("é".repeat(17000));
        *value.pointer_mut(right).unwrap() = json!("x".repeat(34000));
        let e = from_payload(kind, value);
        let rec = round_trip(&mut h, e.clone());
        assert_eq!(
            integer(
                &tx,
                &format!("SELECT detail IS NULL FROM entity_details WHERE family='{family}'")
            ),
            1
        );
        let chunks = refs(&tx, "");
        assert_eq!(chunks.len(), 1);
        let raw = std::fs::read(blob_path(&root, &chunks[0].1)).unwrap();
        let decoded: Value = serde_json::from_slice(&raw).unwrap();
        assert_eq!(
            decoded.pointer(left).unwrap().as_str().unwrap().len(),
            34000
        );
        assert_eq!(
            decoded.pointer(right).unwrap().as_str().unwrap().len(),
            34000
        );
        h.delete(kind, e.id(), rec.revision).unwrap();
    }
    // Exact aggregate byte boundary, without an oversized individual slot.
    for kind in [EntityKind::Agent, EntityKind::Team, EntityKind::Run] {
        let mut base = match kind {
            EntityKind::Agent => serde_json::to_value(agent()).unwrap(),
            EntityKind::Team => serde_json::to_value(team()).unwrap(),
            _ => serde_json::to_value(run()).unwrap(),
        };
        let field = if kind == EntityKind::Run {
            "/workspace_path"
        } else {
            "/name"
        };
        *base.pointer_mut(field).unwrap() = json!("");
        let entity = from_payload(kind, base.clone());
        let rec = round_trip(&mut h, entity.clone());
        let overhead = integer(&tx, "SELECT detail_bytes FROM entity_details") as usize;
        h.delete(kind, entity.id(), rec.revision).unwrap();
        for bytes in [65536, 65537] {
            let mut value = base.clone();
            *value.pointer_mut(field).unwrap() = json!("x".repeat(bytes - overhead));
            let entity = from_payload(kind, value);
            let rec = round_trip(&mut h, entity.clone());
            assert_eq!(
                integer(&tx, "SELECT detail_bytes FROM entity_details"),
                bytes as i64
            );
            assert_eq!(
                integer(&tx, "SELECT detail IS NULL FROM entity_details"),
                i64::from(bytes > 65536)
            );
            assert_eq!(refs(&tx, "").len(), usize::from(bytes > 65536));
            h.delete(kind, entity.id(), rec.revision).unwrap();
        }
    }
    // Real disk objects on both sides of R97's physical object boundary.
    for n in [
        MAX_BLOB_BYTES as usize,
        MAX_BLOB_BYTES as usize + 1,
        2 * MAX_BLOB_BYTES as usize + 1,
    ] {
        let prompt = if n == MAX_BLOB_BYTES as usize + 1 {
            format!("{}é", "x".repeat(n - 2)) // UTF-8 character straddles the 64MiB split.
        } else {
            "x".repeat(n)
        };
        let mut r = run();
        r.prompt = prompt.clone();
        let rec = round_trip(&mut h, Entity::Run(Box::new(r)));
        let chunks = refs(&tx, "/prompt");
        assert_eq!(chunks.len(), n.div_ceil(MAX_BLOB_BYTES as usize));
        let mut actual = Vec::new();
        for (i, (ordinal, hash, size)) in chunks.iter().enumerate() {
            assert_eq!(*ordinal, i as i64);
            let bytes = std::fs::read(blob_path(&root, hash)).unwrap();
            assert_eq!(bytes.len() as i64, *size);
            assert_eq!(
                *size as usize,
                (n - i * MAX_BLOB_BYTES as usize).min(MAX_BLOB_BYTES as usize)
            );
            assert_eq!(hash, &BlobHash::of(&bytes).to_string());
            actual.extend_from_slice(&bytes);
        }
        assert_eq!(actual, prompt.as_bytes());
        if n > 2 * MAX_BLOB_BYTES as usize {
            assert_eq!(chunks[0].1, chunks[1].1);
            assert_ne!(chunks[0].0, chunks[1].0);
        }
        h.delete(EntityKind::Run, "r", rec.revision).unwrap();
    }
    assert_eq!(integer(&tx, "SELECT count(*) FROM entity_details"), 0);
}

// Detects accessing disk before validating metadata, or accepting a damaged
// object/chunk sequence. Each failed live unit is discarded before the next.
#[test]
fn a_missing_corrupt_reordered_or_short_chunk_is_a_stored_data_error() {
    let mut f = Fixture::new();
    let root = f.temp.path().join("data");
    let mut r = run();
    r.prompt = format!("{}y", "x".repeat(MAX_BLOB_BYTES as usize));
    {
        let tx = f.conn.transaction().unwrap();
        let state = FailureState::new();
        round_trip(&mut harness(&tx, &f.disk, &state), Entity::Run(Box::new(r)));
        tx.commit().unwrap();
    }
    for damage in [
        "metadata-first",
        "missing-row",
        "missing-file",
        "corrupt-file",
        "unequal-size-metadata",
        "short",
    ] {
        let tx = f.conn.transaction().unwrap();
        let state = FailureState::new();
        let chunks = refs(&tx, "/prompt");
        let path = blob_path(&root, &chunks[0].1);
        let original = std::fs::read(&path).unwrap();
        match damage {
            "metadata-first" => {
                tx.execute_batch("UPDATE entity_details SET format=2")
                    .unwrap();
                std::fs::remove_file(&path).unwrap();
            }
            "missing-row" => {
                tx.execute_batch(
                    "DELETE FROM entity_blob_refs WHERE locator='/prompt' AND ordinal=0",
                )
                .unwrap();
            }
            "missing-file" => {
                std::fs::remove_file(&path).unwrap();
            }
            "corrupt-file" => {
                std::fs::write(&path, b"wrong").unwrap();
            }
            "unequal-size-metadata" => {
                tx.execute("UPDATE entity_blob_refs SET sha256=CASE ordinal WHEN 0 THEN ?1 ELSE ?2 END,size=CASE ordinal WHEN 0 THEN 1 ELSE ?3 END WHERE locator='/prompt'",rusqlite::params![chunks[1].1,chunks[0].1,MAX_BLOB_BYTES as i64]).unwrap();
            }
            _ => {
                tx.execute_batch(
                    "UPDATE entity_blob_refs SET size=size-1 WHERE locator='/prompt' AND ordinal=0",
                )
                .unwrap();
            }
        }
        let error = harness(&tx, &f.disk, &state)
            .get(EntityKind::Run, "r")
            .unwrap_err();
        if damage == "metadata-first" {
            assert!(
                matches!(
                    &error,
                    SqliteError::EntityCorrupt {
                        check: "detail metadata",
                        ..
                    }
                ),
                "{error:?}"
            );
        }
        stored(error);
        assert!(state.failure().is_some(), "{damage}");
        tx.rollback().unwrap();
        std::fs::write(&path, &original).unwrap();
        let tx = f.conn.transaction().unwrap();
        let state = FailureState::new();
        assert!(
            harness(&tx, &f.disk, &state)
                .get(EntityKind::Run, "r")
                .unwrap()
                .is_some()
        );
    }
}

// Detects projection authority replacing detail authority. Fresh Writer state
// per mutation avoids a previous latch masking a later corrupt column.
#[test]
fn projection_drift_in_each_column_and_member_row_is_a_stored_data_error() {
    let mut f = Fixture::new();
    {
        let tx = f.conn.transaction().unwrap();
        let state = FailureState::new();
        let mut h = harness(&tx, &f.disk, &state);
        for e in entities() {
            round_trip(&mut h, e);
        }
        tx.commit().unwrap();
    }
    for (kind, table, id, columns) in [
        (
            EntityKind::Agent,
            "agents",
            "a",
            "name adapter role current_task last_error config status autonomous tasks_completed tasks_failed total_runs total_runtime_ms tokens_used",
        ),
        (
            EntityKind::Team,
            "teams",
            "t",
            "name description lead_agent_id created_at_text status config created_at updated_at",
        ),
        (
            EntityKind::Run,
            "runs",
            "r",
            "task_id agent_id status attempt started_at_text started_at finished_at error_kind prompt_sha256 tokens_input tokens_output tokens_reasoning tokens_cache_read tokens_cache_write tokens_total local",
        ),
    ] {
        for column in columns.split_whitespace() {
            let tx = f.conn.transaction().unwrap();
            let state = FailureState::new();
            tx.execute_batch(&format!("PRAGMA ignore_check_constraints=ON; UPDATE {table} SET {column}=CASE typeof({column}) WHEN 'integer' THEN {column}+1 WHEN 'text' THEN 'drift' ELSE 0 END")).unwrap();
            stored(harness(&tx, &f.disk, &state).get(kind, id).unwrap_err());
            assert!(state.failure().is_some(), "{table}.{column}");
            tx.rollback().unwrap();
            f.conn
                .execute_batch("PRAGMA ignore_check_constraints=OFF")
                .unwrap();
        }
    }
    for (kind, table, id, fields) in [
        (
            EntityKind::Agent,
            "agents",
            "a",
            "name adapter role current_task last_error",
        ),
        (
            EntityKind::Team,
            "teams",
            "t",
            "name description lead_agent_id created_at_text",
        ),
        (
            EntityKind::Run,
            "runs",
            "r",
            "task_id agent_id started_at_text error",
        ),
    ] {
        for field in fields.split_whitespace() {
            for (suffix, value) in [("form", "1"), ("sha256", "'bad'"), ("bytes", "70000")] {
                let tx = f.conn.transaction().unwrap();
                let state = FailureState::new();
                tx.execute_batch(&format!("PRAGMA ignore_check_constraints=ON; UPDATE {table} SET {field}_{suffix}={value}")).unwrap();
                stored(harness(&tx, &f.disk, &state).get(kind, id).unwrap_err());
                assert!(state.failure().is_some(), "{table}.{field}_{suffix}");
                tx.rollback().unwrap();
                f.conn
                    .execute_batch("PRAGMA ignore_check_constraints=OFF")
                    .unwrap();
            }
        }
    }
    for sql in [
        "UPDATE team_members SET ordinal=ordinal+5",
        "UPDATE team_members SET agent_id='drift'",
        "UPDATE team_members SET role='member' WHERE ordinal=0",
        "UPDATE team_members SET joined_at_text='drift'",
        "UPDATE team_members SET joined_at=123",
        "DELETE FROM team_members WHERE ordinal=1",
        "UPDATE team_tasks SET task_id='drift' WHERE task_id='a'",
        "DELETE FROM team_tasks WHERE task_id='a'",
    ] {
        let tx = f.conn.transaction().unwrap();
        let state = FailureState::new();
        tx.execute_batch(sql).unwrap();
        stored(
            harness(&tx, &f.disk, &state)
                .get(EntityKind::Team, "t")
                .unwrap_err(),
        );
        assert!(state.failure().is_some(), "{sql}");
        tx.rollback().unwrap();
    }
}

// Detects partial persistence after a trigger failure caught by the caller.
#[test]
fn a_failed_step_leaves_the_old_row_and_references_readable_even_when_caught() {
    for family in [EntityKind::Agent, EntityKind::Team, EntityKind::Run] {
        for operation in ["update", "move", "delete"] {
            let mut f = Fixture::new();
            let tx = f.conn.transaction().unwrap();
            let state = FailureState::new();
            let mut h = harness(&tx, &f.disk, &state);
            let entity = entities().into_iter().find(|e| e.kind() == family).unwrap();
            let old = round_trip(&mut h, entity.clone());
            let table = match family {
                EntityKind::Agent => "agents",
                EntityKind::Team => "teams",
                _ => "runs",
            };
            let (trigger, target) = if operation == "update" {
                ("INSERT", "entity_details")
            } else if operation == "delete" {
                ("DELETE", table)
            } else {
                ("UPDATE", table)
            };
            tx.execute_batch(&format!("CREATE TEMP TRIGGER refuse_step BEFORE {trigger} ON {target} BEGIN SELECT RAISE(ABORT,'synthetic'); END")).unwrap();
            let result = match operation {
                "update" => h.update(entity.clone(), old.revision).map(|_| ()),
                "move" => h.move_to_end(family, entity.id(), old.revision).map(|_| ()),
                _ => h.delete(family, entity.id(), old.revision),
            };
            assert!(result.is_err(), "{table} {operation}");
            assert_eq!(h.get(family, entity.id()).unwrap(), Some(old.clone()));
            h.ensure_committable().unwrap();
            tx.execute_batch("DROP TRIGGER refuse_step").unwrap();
            tx.commit().unwrap();
            let tx = f.conn.transaction().unwrap();
            let state = FailureState::new();
            assert_eq!(
                harness(&tx, &f.disk, &state)
                    .get(family, entity.id())
                    .unwrap(),
                Some(old)
            );
        }
    }
}

// Detects hydration on a newly opened view rather than the selecting WAL snapshot.
#[test]
fn snapshot_and_unit_of_work_hydrate_one_state() {
    let mut f = Fixture::new();
    {
        let tx = f.conn.transaction().unwrap();
        let state = FailureState::new();
        for e in entities() {
            round_trip(&mut harness(&tx, &f.disk, &state), e);
        }
        tx.commit().unwrap();
    }
    let mut reader = Connection::open(f.temp.path().join("corpus.db")).unwrap();
    let snapshot = reader.transaction().unwrap();
    let state = FailureState::new();
    let diagnostic = NativeFormatHarness::new(
        &snapshot,
        &f.disk,
        "p",
        123,
        state.clone(),
        FailureSource::Reader,
    );
    let selected: Vec<Record> = entities()
        .iter()
        .map(|e| diagnostic.get(e.kind(), e.id()).unwrap().unwrap())
        .collect();
    {
        let tx = f.conn.transaction().unwrap();
        let state = FailureState::new();
        let mut h = harness(&tx, &f.disk, &state);
        for old in &selected {
            h.delete(old.entity.kind(), old.entity.id(), old.revision)
                .unwrap();
        }
        tx.commit().unwrap();
    }
    for old in selected {
        assert_eq!(
            diagnostic
                .hydrate(old.entity.kind(), &[old.entity.id().into()])
                .unwrap(),
            [old]
        );
    }
    snapshot.commit().unwrap();
    let tx = f.conn.transaction().unwrap();
    let state = FailureState::new();
    let mut h = harness(&tx, &f.disk, &state);
    for e in entities() {
        let old = round_trip(&mut h, e.clone());
        let moved = h.move_to_end(e.kind(), e.id(), old.revision).unwrap();
        assert_eq!(h.hydrate(e.kind(), &[e.id().into()]).unwrap(), [moved]);
    }
}

// Detects unchecked stale revisions, position reuse or counter overflow writes.
#[test]
fn revision_races_and_position_moves_use_checked_counters() {
    for entity in entities() {
        let mut f = Fixture::new();
        let tx = f.conn.transaction().unwrap();
        let state = FailureState::new();
        let mut h = harness(&tx, &f.disk, &state);
        let old = round_trip(&mut h, entity.clone());
        let updated = h.update(entity.clone(), old.revision).unwrap();
        assert_eq!(updated.position, old.position);
        assert_eq!(updated.revision.get(), 2);
        assert!(
            matches!(h.update(entity.clone(), old.revision), Err(SqliteError::Store(StoreError::StaleRevision { expected, actual: Some(actual), .. })) if expected == old.revision && actual == updated.revision)
        );
        assert!(
            matches!(h.move_to_end(entity.kind(), entity.id(), old.revision), Err(SqliteError::Store(StoreError::StaleRevision { expected, actual: Some(actual), .. })) if expected == old.revision && actual == updated.revision)
        );
        assert!(
            matches!(h.delete(entity.kind(), entity.id(), old.revision), Err(SqliteError::Store(StoreError::StaleRevision { expected, actual: Some(actual), .. })) if expected == old.revision && actual == updated.revision)
        );
        let moved = h
            .move_to_end(entity.kind(), entity.id(), updated.revision)
            .unwrap();
        assert_eq!(moved.position, old.position + 1);
        assert_eq!(moved.revision.get(), 3);
        h.delete(entity.kind(), entity.id(), moved.revision)
            .unwrap();
        let recreated = h.create(entity.clone()).unwrap();
        assert_eq!(recreated.position, moved.position + 1);
        tx.execute_batch("UPDATE counters SET value=9223372036854775807")
            .unwrap();
        assert!(matches!(
            h.move_to_end(entity.kind(), entity.id(), recreated.revision),
            Err(SqliteError::Store(StoreError::CounterExhausted))
        ));
        assert_eq!(h.get(entity.kind(), entity.id()).unwrap(), Some(recreated));
        let table = match entity.kind() {
            EntityKind::Agent => "agents",
            EntityKind::Team => "teams",
            _ => "runs",
        };
        tx.execute_batch(&format!("UPDATE {table} SET revision=9223372036854775807"))
            .unwrap();
        let max: Revision = serde_json::from_value(json!(i64::MAX)).unwrap();
        let before = tx.total_changes();
        assert!(matches!(
            h.update(entity.clone(), max),
            Err(SqliteError::Store(StoreError::CounterExhausted))
        ));
        assert!(matches!(
            h.move_to_end(entity.kind(), entity.id(), max),
            Err(SqliteError::Store(StoreError::CounterExhausted))
        ));
        assert_eq!(tx.total_changes(), before);
        h.ensure_committable().unwrap();
        assert!(state.failure().is_none());
    }
}

// N5: a full reader validates even fields that the mutation would replace.
#[test]
fn native_crud_never_repairs_away_unreadable_old_detail() {
    for entity in entities() {
        for operation in ["get", "update", "move", "delete"] {
            let mut f = Fixture::new();
            let tx = f.conn.transaction().unwrap();
            let state = FailureState::new();
            round_trip(&mut harness(&tx, &f.disk, &state), entity.clone());
            tx.execute_batch("UPDATE entity_details SET format=2")
                .unwrap();
            let before = tx.total_changes();
            let mut h = harness(&tx, &f.disk, &state);
            let result = match operation {
                "get" => h.get(entity.kind(), entity.id()).map(|_| ()),
                "update" => h.update(entity.clone(), Revision::ONE).map(|_| ()),
                "move" => h
                    .move_to_end(entity.kind(), entity.id(), Revision::ONE)
                    .map(|_| ()),
                _ => h.delete(entity.kind(), entity.id(), Revision::ONE),
            };
            stored(result.unwrap_err());
            assert_eq!(tx.total_changes(), before);
            assert!(h.ensure_committable().is_err());
            assert!(state.failure().is_some());
        }
    }
}

// The tuple belongs to the claim path, not payload attempt. M17 requires an
// unclaimed task reassignment to remain legal; claimed identity stays fixed.
#[test]
fn a_claimed_and_an_unclaimed_run_obey_the_two_case_check_and_crud_never_allocates_or_reassigns() {
    let mut f = Fixture::new();
    let tx = f.conn.transaction().unwrap();
    let state = FailureState::new();
    let mut h = harness(&tx, &f.disk, &state);
    let original = round_trip(&mut h, Entity::Run(Box::new(run())));
    let tuple = |tx: &Transaction<'_>| {
        tx.query_row(
            "SELECT run_no,counted,epoch,attempt_no FROM runs",
            [],
            |r| {
                Ok((
                    r.get::<_, Option<i64>>(0)?,
                    r.get::<_, Option<i64>>(1)?,
                    r.get::<_, Option<i64>>(2)?,
                    r.get::<_, Option<i64>>(3)?,
                ))
            },
        )
        .unwrap()
    };
    assert_eq!(tuple(&tx), (None, None, None, None));
    let mut reassigned = run();
    reassigned.task_id = "another-task".into();
    reassigned.agent_id = "another-agent".into();
    let updated = h
        .update(Entity::Run(Box::new(reassigned.clone())), original.revision)
        .unwrap();
    let moved = h
        .move_to_end(EntityKind::Run, "r", updated.revision)
        .unwrap();
    assert_eq!(tuple(&tx), (None, None, None, None));
    assert_eq!(h.get(EntityKind::Run, "r").unwrap(), Some(moved.clone()));
    // Direct SQL installs synthetic claimed fixtures, not a second claim writer.
    // Allocation by the real claim path remains T12's separately owned gate.
    for (counted, attempt) in [(0, None), (1, Some(7))] {
        tx.execute(
            "UPDATE runs SET run_no=41,counted=?1,epoch=43,attempt_no=?2",
            rusqlite::params![counted, attempt],
        )
        .unwrap();
        assert_eq!(tuple(&tx), (Some(41), Some(counted), Some(43), attempt));
        let claimed = h.get(EntityKind::Run, "r").unwrap().unwrap();
        for field in ["task", "agent"] {
            let mut invalid = reassigned.clone();
            if field == "task" {
                invalid.task_id = "refused".into();
            } else {
                invalid.agent_id = "refused".into();
            }
            assert!(
                matches!(h.update(Entity::Run(Box::new(invalid)), claimed.revision),
                Err(SqliteError::Unrepresentable(reason)) if reason == "claimed run identity")
            );
            assert_eq!(tuple(&tx), (Some(41), Some(counted), Some(43), attempt));
        }
        let mut payload = reassigned.clone();
        payload.attempt = 2;
        let current = h.get(EntityKind::Run, "r").unwrap().unwrap();
        let updated = h
            .update(Entity::Run(Box::new(payload)), current.revision)
            .unwrap();
        let moved = h
            .move_to_end(EntityKind::Run, "r", updated.revision)
            .unwrap();
        assert_eq!(tuple(&tx), (Some(41), Some(counted), Some(43), attempt));
        assert_eq!(h.get(EntityKind::Run, "r").unwrap(), Some(moved));
        // Partial tuples and invalid counted states fail without changing a row.
        for sql in [
            "UPDATE runs SET epoch=NULL",
            "UPDATE runs SET run_no=NULL",
            "UPDATE runs SET counted=NULL",
            "UPDATE runs SET counted=2",
            "UPDATE runs SET counted=0,attempt_no=7",
            "UPDATE runs SET counted=1,attempt_no=NULL",
        ] {
            assert!(tx.execute_batch(sql).is_err(), "{sql}");
            assert_eq!(tuple(&tx), (Some(41), Some(counted), Some(43), attempt));
        }
    }
    let current = h.get(EntityKind::Run, "r").unwrap().unwrap();
    h.delete(EntityKind::Run, "r", current.revision).unwrap();
    assert_eq!(integer(&tx, "SELECT count(*) FROM runs"), 0);
    assert_eq!(integer(&tx, "SELECT count(*) FROM task_epochs"), 0);
}

#[test]
fn orphan_detail_reference_member_and_task_rows_cannot_be_replaced_by_create() {
    for orphan in ["detail", "reference", "member", "task"] {
        let mut f = Fixture::new();
        let tx = f.conn.transaction().unwrap();
        let state = FailureState::new();
        let e = if matches!(orphan, "member" | "task") {
            Entity::Team(Box::new(team()))
        } else {
            Entity::Run(Box::new(run()))
        };
        let mut h = harness(&tx, &f.disk, &state);
        round_trip(&mut h, e.clone());
        if matches!(orphan, "member" | "task") {
            tx.execute_batch("DELETE FROM teams; DELETE FROM entity_details")
                .unwrap();
        } else {
            tx.execute_batch("DELETE FROM runs").unwrap();
        }
        match orphan {
            "reference" => tx.execute_batch("DELETE FROM entity_details").unwrap(),
            "member" => tx.execute_batch("DELETE FROM team_tasks").unwrap(),
            "task" => tx.execute_batch("DELETE FROM team_members").unwrap(),
            _ => (),
        }
        let before = tx.total_changes();
        stored(h.create(e).unwrap_err());
        assert_eq!(tx.total_changes(), before, "{orphan}");
        assert!(state.failure().is_some());
        assert!(h.ensure_committable().is_err());
    }
}

#[test]
fn nonpositive_positions_refuse_reads_and_new_position_allocation() {
    for kind in [EntityKind::Agent, EntityKind::Team, EntityKind::Run] {
        for operation in ["read", "create"] {
            let mut f = Fixture::new();
            let tx = f.conn.transaction().unwrap();
            let state = FailureState::new();
            let e = entities().into_iter().find(|e| e.kind() == kind).unwrap();
            let mut h = harness(&tx, &f.disk, &state);
            round_trip(&mut h, e.clone());
            let table = match kind {
                EntityKind::Agent => "agents",
                EntityKind::Team => "teams",
                _ => "runs",
            };
            tx.execute_batch(&format!("UPDATE {table} SET position=0"))
                .unwrap();
            let before = tx.total_changes();
            let error = if operation == "read" {
                h.get(kind, e.id()).unwrap_err()
            } else {
                let mut value = serde_json::to_value(e).unwrap();
                value["value"]["id"] = json!("new");
                h.create(serde_json::from_value(value).unwrap())
                    .unwrap_err()
            };
            stored(error);
            assert_eq!(tx.total_changes(), before);
            assert!(state.failure().is_some());
        }
    }
}

// Independent SQL + filesystem oracle; do not ask the production reader to
// establish chunk boundaries, ordering, deduplication or the whole-value tag.
fn assert_physical(tx: &Transaction<'_>, root: &std::path::Path, locator: &str, expected: &[u8]) {
    let chunks = refs(tx, locator);
    let max = MAX_BLOB_BYTES as usize;
    assert_eq!(chunks.len(), expected.len().div_ceil(max), "{locator}");
    for ((ordinal, hash, size), (i, part)) in chunks.iter().zip(expected.chunks(max).enumerate()) {
        assert_eq!(*ordinal, i as i64, "{locator}");
        assert_eq!(*size, part.len() as i64, "{locator} ordinal {i}");
        assert_eq!(hash, &BlobHash::of(part).to_string());
        assert_eq!(std::fs::read(blob_path(root, hash)).unwrap(), part);
        assert_eq!(
            tx.query_row(
                "SELECT size FROM blobs WHERE project_id='p' AND sha256=?1",
                [hash],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            *size
        );
    }
    if locator.is_empty() {
        let (inline, size, hash): (bool, i64, String) = tx
            .query_row(
                "SELECT detail IS NULL,detail_bytes,detail_sha256 FROM entity_details",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert!(inline);
        assert_eq!(size, expected.len() as i64);
        assert_eq!(hash, BlobHash::of(expected).to_string());
    } else {
        let raw: String = tx
            .query_row("SELECT detail FROM entity_details", [], |r| r.get(0))
            .unwrap();
        let detail: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(
            detail.pointer(locator).unwrap(),
            &json!({"$blob":{
            "sha256":BlobHash::of(expected).to_string(),"bytes":expected.len(),"chunks":chunks.len()}})
        );
    }
}

#[test]
fn every_variable_slot_and_environment_name_crosses_the_physical_disk_boundary() {
    let max = MAX_BLOB_BYTES as usize;
    for (kind, family, base, mut slots) in text_cases() {
        if kind == EntityKind::Agent {
            slots.push("environment-name");
        }
        for slot in slots {
            // One fixture per slot: exited objects cannot accumulate across the
            // matrix. The largest value is 128MiB+1 and repeated objects dedup.
            let mut f = Fixture::new();
            for n in [max, max + 1, 2 * max + 1] {
                eprintln!("physical {family} {slot} {n}");
                let text = if n == max + 1 {
                    format!("{}é", "x".repeat(max - 1))
                } else {
                    "x".repeat(n)
                };
                let mut value = base.clone();
                let locator = match slot {
                    "environment-name" => {
                        value["config"]["env"] = json!({text.clone(): "v"});
                        "/config/env/0/0"
                    }
                    "/config/env/k" => {
                        *value.pointer_mut(slot).unwrap() = json!(text);
                        "/config/env/0/1"
                    }
                    _ => {
                        *value.pointer_mut(slot).unwrap() = json!(text);
                        slot
                    }
                };
                let entity = from_payload(kind, value);
                let tx = f.conn.transaction().unwrap();
                let state = FailureState::new();
                let mut h = harness(&tx, &f.disk, &state);
                let record = h.create(entity.clone()).unwrap();
                assert_eq!(h.get(kind, entity.id()).unwrap(), Some(record.clone()));
                assert_eq!(record.entity, entity);
                assert_physical(&tx, &f.temp.path().join("data"), locator, text.as_bytes());
                if slot == "/id" {
                    let (form, key): (i64, String) = tx
                        .query_row("SELECT key_form,entity_key FROM entity_details", [], |r| {
                            Ok((r.get(0)?, r.get(1)?))
                        })
                        .unwrap();
                    assert_eq!((form, key), (1, BlobHash::of(text.as_bytes()).to_string()));
                }
                if n > 2 * max {
                    let chunks = refs(&tx, locator);
                    assert_eq!(chunks[0].1, chunks[1].1);
                    assert_ne!(chunks[0].0, chunks[1].0);
                }
                h.ensure_committable().unwrap();
                assert!(state.failure().is_none());
                tx.rollback().unwrap();
            }
        }
    }
}

fn aggregate_entity(kind: EntityKind, bytes: usize) -> Entity {
    // Every slot stays inline. 65533 bytes + JSON quotes/comma = 65536,
    // so interior 64MiB objects repeat byte-for-byte for the larger case.
    let mut value = match kind {
        EntityKind::Agent => serde_json::to_value(agent()).unwrap(),
        _ => serde_json::to_value(team()).unwrap(),
    };
    let field = if kind == EntityKind::Agent {
        "capabilities"
    } else {
        "task_pool"
    };
    let count = (bytes / 65536 - 1).max(1);
    value[field] = json!(vec!["x".repeat(65533); count]);
    let current = serde_json::to_vec(&value).unwrap().len();
    if bytes - current - 3 > 65536 {
        value[field]
            .as_array_mut()
            .unwrap()
            .push(json!("x".repeat((bytes - current - 3) / 2)));
    }
    let current = serde_json::to_vec(&value).unwrap().len();
    value[field]
        .as_array_mut()
        .unwrap()
        .push(json!("x".repeat(bytes - current - 3)));
    assert_eq!(serde_json::to_vec(&value).unwrap().len(), bytes);
    from_payload(kind, value)
}

#[test]
fn aggregate_detail_crosses_the_physical_boundary_without_an_oversized_slot() {
    let max = MAX_BLOB_BYTES as usize;
    for kind in [EntityKind::Agent, EntityKind::Team] {
        for n in [max, max + 1, max + 1024, 3 * max + 65536 + 1024] {
            let mut f = Fixture::new();
            eprintln!("aggregate {kind:?} {n}");
            let mut entity = aggregate_entity(kind, n);
            // Place é at MAX-1 inside an interior string, not at a JSON
            // delimiter; this also proves aggregate UTF-8 decodes after join.
            if n == max + 1024 {
                // Serialize the underlying domain value.
                let value = match &entity {
                    Entity::Agent(a) => serde_json::to_value(a).unwrap(),
                    Entity::Team(t) => serde_json::to_value(t).unwrap(),
                    _ => unreachable!(),
                };
                let mut raw = serde_json::to_vec(&value).unwrap();
                assert_eq!(&raw[max - 1..max + 1], b"xx");
                raw[max - 1..max + 1].copy_from_slice("é".as_bytes());
                entity = from_payload(kind, serde_json::from_slice(&raw).unwrap());
            }
            let expected = match &entity {
                Entity::Agent(a) => serde_json::to_vec(a).unwrap(),
                Entity::Team(t) => serde_json::to_vec(t).unwrap(),
                _ => unreachable!(),
            };
            assert_eq!(expected.len(), n);
            let tx = f.conn.transaction().unwrap();
            let state = FailureState::new();
            let mut h = harness(&tx, &f.disk, &state);
            let record = h.create(entity.clone()).unwrap();
            assert_eq!(h.get(kind, entity.id()).unwrap(), Some(record));
            assert_physical(&tx, &f.temp.path().join("data"), "", &expected);
            assert_eq!(
                integer(&tx, "SELECT count(DISTINCT locator) FROM entity_blob_refs"),
                1
            );
            if n > 3 * max {
                let chunks = refs(&tx, "");
                assert_eq!(chunks[1].1, chunks[2].1);
            }
            h.ensure_committable().unwrap();
            assert!(state.failure().is_none());
        }
    }
}

#[test]
fn equal_size_distinct_chunk_reordering_is_corrupt_and_latches_the_writer() {
    let mut f = Fixture::new();
    let mut a = agent();
    a.config.system_prompt = Some(format!(
        "{}{}",
        "x".repeat(MAX_BLOB_BYTES as usize),
        "y".repeat(MAX_BLOB_BYTES as usize)
    ));
    let tx = f.conn.transaction().unwrap();
    let state = FailureState::new();
    harness(&tx, &f.disk, &state)
        .create(Entity::Agent(Box::new(a)))
        .unwrap();
    let chunks = refs(&tx, "/config/system_prompt");
    assert_eq!(chunks.len(), 2);
    assert_eq!(chunks[0].2, chunks[1].2);
    assert_ne!(chunks[0].1, chunks[1].1);
    tx.execute("UPDATE entity_blob_refs SET sha256=CASE ordinal WHEN 0 THEN ?1 ELSE ?2 END WHERE locator='/config/system_prompt'",rusqlite::params![chunks[1].1,chunks[0].1]).unwrap();
    let h = harness(&tx, &f.disk, &state);
    let error = match h.get(EntityKind::Agent, "a") {
        Err(error) => error,
        Ok(_) => panic!("equal-size reordered bytes were accepted"),
    };
    assert!(
        matches!(&error,SqliteError::EntityCorrupt{check:"referenced bytes",locator,..} if locator=="/config/system_prompt"),
        "{error:?}"
    );
    assert!(state.failure().is_some());
    assert!(h.ensure_committable().is_err());
}

// The root and a nested single object use the same stored-data contract as
// chunked values. Every damage gets its own live Writer/latch and rollback.
#[test]
fn single_object_and_aggregate_damage_refuse_reads_with_bounded_diagnostics() {
    for shape in ["nested", "agent-root", "team-root", "run-root"] {
        let entity = match shape {
            "nested" => {
                let mut a = agent();
                a.config.system_prompt = Some("x".repeat(65537));
                Entity::Agent(Box::new(a))
            }
            "agent-root" => aggregate_entity(EntityKind::Agent, 68000),
            "team-root" => aggregate_entity(EntityKind::Team, 68000),
            _ => {
                let mut r = run();
                r.workspace_path = "x".repeat(34000);
                r.started_at = "y".repeat(34000);
                Entity::Run(Box::new(r))
            }
        };
        let kind = entity.kind();
        let id = entity.id().to_string();
        let locator = if shape == "nested" {
            "/config/system_prompt"
        } else {
            ""
        };
        let mut f = Fixture::new();
        {
            let tx = f.conn.transaction().unwrap();
            let state = FailureState::new();
            harness(&tx, &f.disk, &state).create(entity).unwrap();
            tx.commit().unwrap();
        }
        for damage in [
            "metadata-first",
            "missing-row",
            "missing-file",
            "corrupt-file",
            "short",
            "extra-row",
            "untagged-row",
            "tag-without-rows",
            "untrusted-locator",
        ] {
            let tx = f.conn.transaction().unwrap();
            let state = FailureState::new();
            let chunks = refs(&tx, locator);
            assert_eq!(chunks.len(), 1);
            let path = blob_path(&f.temp.path().join("data"), &chunks[0].1);
            let original = std::fs::read(&path).unwrap();
            match damage {
                "metadata-first" => {
                    tx.execute_batch("UPDATE entity_details SET format=2")
                        .unwrap();
                    std::fs::remove_file(&path).unwrap();
                }
                "missing-row" | "tag-without-rows" => {
                    tx.execute("DELETE FROM entity_blob_refs WHERE locator=?1", [locator])
                        .unwrap();
                }
                "missing-file" => {
                    std::fs::remove_file(&path).unwrap();
                }
                "corrupt-file" => {
                    std::fs::write(&path, b"PRIVATE-SYNTHETIC-CONTENT").unwrap();
                }
                "short" => {
                    tx.execute(
                        "UPDATE entity_blob_refs SET size=size-1 WHERE locator=?1",
                        [locator],
                    )
                    .unwrap();
                }
                "extra-row" => {
                    tx.execute("INSERT INTO entity_blob_refs SELECT project_id,family,key_form,entity_key,locator,ordinal+1,sha256,size FROM entity_blob_refs WHERE locator=?1",[locator]).unwrap();
                }
                "untagged-row" => {
                    // Keep the authoritative payload/projections unchanged:
                    // /name (or /workspace_path) is an inline slot with no tag.
                    let inline = if kind == EntityKind::Run {
                        "/workspace_path"
                    } else {
                        "/name"
                    };
                    tx.execute("INSERT INTO entity_blob_refs SELECT project_id,family,key_form,entity_key,?1,ordinal,sha256,size FROM entity_blob_refs WHERE locator=?2",rusqlite::params![inline,locator]).unwrap();
                }
                _ => {
                    let hostile = format!("/PRIVATE-SYNTHETIC-{}", "x".repeat(65536));
                    tx.execute(
                        "INSERT INTO entity_blob_refs SELECT project_id,family,key_form,entity_key,?1,ordinal,sha256,size FROM entity_blob_refs WHERE locator=?2",
                        rusqlite::params![hostile, locator],
                    )
                    .unwrap();
                }
            }
            let h = harness(&tx, &f.disk, &state);
            let error = h.get(kind, &id).unwrap_err();
            match &error {
                SqliteError::EntityCorrupt { check, locator, .. } => {
                    assert!(check.len() <= 64, "{shape} {damage}");
                    assert!(locator.len() <= 64, "{shape} {damage}");
                    assert!(!locator.contains("PRIVATE-SYNTHETIC"));
                    if damage == "metadata-first" {
                        assert_eq!(*check, "detail metadata");
                    }
                }
                _ => panic!("{shape} {damage}: {error:?}"),
            }
            let rendered = error.to_string();
            assert!(rendered.len() < 256);
            assert!(!rendered.contains("PRIVATE-SYNTHETIC"));
            assert!(state.failure().is_some(), "{shape} {damage}");
            assert!(h.ensure_committable().is_err());
            tx.rollback().unwrap();
            std::fs::write(&path, &original).unwrap();
            let tx = f.conn.transaction().unwrap();
            let state = FailureState::new();
            let h = harness(&tx, &f.disk, &state);
            assert!(
                h.get(kind, &id).unwrap().is_some(),
                "restored {shape} {damage}"
            );
            h.ensure_committable().unwrap();
        }
    }
}

// Run has a fixed set of scalar fields, unlike Agent/Team collections. After
// per-slot spilling, a >64MiB root is unreachable. Maximal JSON escaping of
// every inline variable field stays below 4MiB; crossing the slot threshold
// replaces those fields with small tags. Keep the real root assertions here.
#[test]
fn run_aggregate_is_bounded_after_slot_spilling_even_with_maximal_json_escaping() {
    for n in [65536, 65537] {
        let mut r = run();
        let text = "\u{0001}".repeat(n);
        r.id = text.clone();
        r.task_id = text.clone();
        r.agent_id = text.clone();
        r.started_at = text.clone();
        r.finished_at = Some(text.clone());
        r.workspace_path = text.clone();
        r.error = Some(text.clone());
        r.prompt = text;
        r.pid = Some(u32::MAX);
        r.attempt = u32::MAX;
        r.tokens = Some(orch_domain::TokenUsage {
            input: u64::MAX,
            output: u64::MAX,
            reasoning: u64::MAX,
            total: u64::MAX,
            cache_read: u64::MAX,
            cache_write: u64::MAX,
        });
        r.error_kind = Some(orch_domain::AdapterErrorKind::Unknown);
        let entity = Entity::Run(Box::new(r));
        let mut f = Fixture::new();
        let tx = f.conn.transaction().unwrap();
        let state = FailureState::new();
        let mut h = harness(&tx, &f.disk, &state);
        let record = h.create(entity.clone()).unwrap();
        assert_eq!(h.get(EntityKind::Run, entity.id()).unwrap(), Some(record));
        let bytes = integer(&tx, "SELECT detail_bytes FROM entity_details");
        if n == 65536 {
            assert!(bytes > 7 * 65536 * 6);
            assert!(bytes < 4 * 1024 * 1024);
            let chunks = refs(&tx, "");
            assert_eq!(chunks.len(), 1);
            let raw = std::fs::read(blob_path(&f.temp.path().join("data"), &chunks[0].1)).unwrap();
            assert_physical(&tx, &f.temp.path().join("data"), "", &raw);
        } else {
            assert!(bytes < 65536);
            assert!(refs(&tx, "").is_empty());
        }
        h.ensure_committable().unwrap();
        assert!(state.failure().is_none());
    }
}
