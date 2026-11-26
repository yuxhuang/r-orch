//! Message mapping on the real schema and the real disk blob store.
use std::path::PathBuf;

use orch_core::store::{Caller, Entity, Filter, Principal, Record, Revision, Sort};
use orch_domain::{Message, MessageChannel, MessageStatus};
use rusqlite::{Connection, params};

use super::{MessageReader, MessageSelection, Messages};
use crate::blob::{BlobHash, DiskBlobStore};
use crate::sqlite::{SqliteError, migrations, test_data};

mod damage;
mod large;
mod pages;

pub(super) struct Rig {
    pub(super) temp: tempfile::TempDir,
    pub(super) _data: crate::data_dir::DataDir,
    pub(super) blobs: DiskBlobStore,
    pub(super) conn: Connection,
}

pub(super) fn rig() -> Rig {
    let (temp, data) = test_data();
    let blobs = DiskBlobStore::open(data.root()).unwrap();
    let conn = Connection::open_in_memory().unwrap();
    for step in [
        migrations::INITIAL_SCHEMA,
        migrations::ATTENTION_FACTS,
        migrations::TASK_FORMAT,
        migrations::AGENT_TEAM_RUN,
        migrations::CONTEXT_CONFIG_TEMPLATE_GOAL,
        migrations::MESSAGE_FORMAT,
    ] {
        conn.execute_batch(step).unwrap();
    }
    Rig {
        temp,
        _data: data,
        blobs,
        conn,
    }
}

pub(super) fn caller(project: &str) -> Caller {
    Caller {
        project: project.into(),
        principal: Principal::Owner {
            identity: "owner".into(),
        },
    }
}

pub(super) fn message(id: &str) -> Message {
    Message {
        id: id.into(),
        channel: MessageChannel::Direct,
        from_agent_id: "agt_1".into(),
        to_agent_id: Some("agt_2".into()),
        subject: "subject".into(),
        body: "body".into(),
        created_at: "2026-11-20T10:00:00.000Z".into(),
        expires_at: None,
        status: MessageStatus::Pending,
        delivered_at: None,
        team_id: None,
        reply_to: None,
    }
}

pub(super) fn text_of(bytes: usize) -> String {
    "a".repeat(bytes)
}

/// The R140 error's key form, check and locator.
pub(super) fn damage<T: std::fmt::Debug>(
    result: crate::sqlite::Result<T>,
) -> (i64, &'static str, &'static str) {
    match result {
        Err(SqliteError::MessageCorrupt {
            form,
            check,
            locator,
        }) => (form, check, locator),
        other => panic!("expected stored-data corruption, got {other:?}"),
    }
}

pub(super) fn id_of(record: &Record) -> &str {
    record.entity.id()
}

pub(super) fn body_of(record: &Record) -> &str {
    match &record.entity {
        Entity::Message(m) => &m.body,
        _ => unreachable!(),
    }
}

impl Rig {
    /// One committed transaction of the given project.
    pub(super) fn commit<T>(
        &mut self,
        project: &str,
        work: impl FnOnce(&mut Messages<'_, '_>) -> T,
    ) -> T {
        let who = caller(project);
        let tx = self.conn.transaction().unwrap();
        let value = work(&mut Messages::new(&tx, &who, &self.blobs));
        tx.commit().unwrap();
        value
    }

    pub(super) fn create(&mut self, project: &str, message: &Message) -> Record {
        self.commit(project, |m| m.create(message, 7).unwrap())
    }

    pub(super) fn reader(&self, project: &'static str) -> MessageReader<'_> {
        MessageReader::new(&self.conn, project, &self.blobs)
    }

    pub(super) fn get(&self, project: &'static str, id: &str) -> Option<Record> {
        self.reader(project).get(id).unwrap()
    }

    pub(super) fn ids(
        &self,
        filter: &Filter,
        sort: Sort,
        after: Option<&Record>,
        limit: usize,
    ) -> (Vec<String>, u64, bool) {
        let page = self
            .reader("p")
            .select(MessageSelection { filter, sort }, after, limit)
            .unwrap();
        let ids = page.items.iter().map(|r| id_of(r).to_owned()).collect();
        (ids, page.total, page.has_more)
    }

    pub(super) fn object_path(&self, hash: &str) -> PathBuf {
        self.temp
            .path()
            .join("data/blobs/sha256")
            .join(&hash[0..2])
            .join(&hash[2..4])
            .join(hash)
    }

    pub(super) fn counter(&self, project: &str) -> Option<i64> {
        self.conn
            .query_row(
                "SELECT value FROM counters WHERE project_id=?1 AND name='position:message'",
                [project],
                |r| r.get(0),
            )
            .ok()
    }

    pub(super) fn text(&self, sql: &str) -> String {
        self.conn.query_row(sql, [], |r| r.get(0)).unwrap()
    }

    pub(super) fn count(&self, table: &str) -> i64 {
        self.conn
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    }
}

fn pending(recipient: &str, now: &str) -> Filter {
    Filter::PendingMessages {
        recipient: recipient.into(),
        now: now.into(),
    }
}

#[test]
fn a_message_round_trips_every_field_with_presence_and_original_times() {
    let mut rig = rig();
    let mut full = message("full");
    full.channel = MessageChannel::Lead;
    full.created_at = "2026-11-20T10:00:00+02:00".into();
    full.expires_at = Some("2026-11-21T10:00:00Z".into());
    full.delivered_at = Some("x".into());
    full.team_id = Some("team".into());
    full.reply_to = Some("msg_0".into());
    let mut broadcast = message("broadcast");
    broadcast.channel = MessageChannel::Broadcast;
    broadcast.to_agent_id = None;
    let mut empties = message("empties");
    empties.to_agent_id = Some(String::new());
    empties.subject = String::new();
    empties.body = String::new();
    empties.expires_at = Some(String::new());
    empties.delivered_at = Some(String::new());
    empties.team_id = Some(String::new());
    empties.reply_to = Some(String::new());
    let mut opaque = message("opaque");
    opaque.created_at = "not a date".into();
    opaque.expires_at = Some("2026-13-45".into());
    let mut fine = message("fine");
    fine.created_at = "2026-11-20T10:00:00.0001Z".into();
    fine.expires_at = Some("2026-11-20T10:00:00.1230000Z".into());
    let mut same = message("same-instant");
    same.created_at = "2026-11-20T12:00:00+02:00".into();
    let mut utc = message("utc-instant");
    utc.created_at = "2026-11-20T10:00:00Z".into();
    let odd = message("n\u{0}ul-\u{e9}\u{1f600}");
    let all = [full, broadcast, empties, opaque, fine, same, utc, odd];
    for (i, m) in all.iter().enumerate() {
        let saved = rig.create("p", m);
        assert_eq!(
            (saved.revision, saved.position),
            (Revision::ONE, i as u64 + 1)
        );
    }
    for m in &all {
        let got = rig.get("p", &m.id).unwrap();
        assert_eq!(got.entity, Entity::Message(Box::new(m.clone())), "{}", m.id);
    }
    // The detail is authoritative and presence is meaning: null against empty.
    let detail = rig.text("SELECT detail FROM entity_details WHERE entity_key='broadcast'");
    assert_eq!(
        detail,
        r#"{"id":"broadcast","channel":"broadcast","from_agent_id":"agt_1","to_agent_id":null,"subject":"subject","body":"body","created_at":"2026-11-20T10:00:00.000Z","status":"pending"}"#
    );
    let nullable: Vec<(String, Option<String>)> = rig
        .conn
        .prepare(
            "SELECT id,to_agent_id FROM messages WHERE id IN ('broadcast','empties') ORDER BY id",
        )
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        nullable,
        [
            ("broadcast".into(), None),
            ("empties".into(), Some(String::new()))
        ]
    );
    // Integer milliseconds only when exact; two spellings of one instant stay two texts.
    let ms = |id: &str| -> (Option<i64>, Option<i64>, String) {
        rig.conn
            .query_row(
                "SELECT created_at,expires_at,created_at_text FROM messages WHERE id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap()
    };
    assert_eq!(ms("opaque").0, None);
    assert_eq!(ms("opaque").1, None);
    assert_eq!(
        ms("fine"),
        (
            None,
            Some(1_795_168_800_123),
            "2026-11-20T10:00:00.0001Z".into()
        )
    );
    assert_eq!(ms("same-instant").0, ms("utc-instant").0);
    assert_ne!(ms("same-instant").2, ms("utc-instant").2);
    // Another project does not see them, and the same id there is independent.
    assert!(rig.get("q", "full").is_none());
    let other = rig.create("q", &message("full"));
    assert_eq!(other.position, 1);
    assert_eq!(rig.get("p", "full").unwrap().position, 1);
}

#[test]
fn the_millisecond_projection_covers_exactly_the_documented_range() {
    use super::{MAX_MILLIS, MIN_MILLIS, millis};
    use rusqlite::types::Value::{Integer, Null};
    assert_eq!(
        millis(Some("0000-01-01T00:00:00.000Z")),
        Integer(MIN_MILLIS)
    );
    assert_eq!(
        millis(Some("9999-12-30T22:00:00.999Z")),
        Integer(MAX_MILLIS)
    );
    assert_eq!(millis(Some("9999-12-30T22:00:01Z")), Null);
    assert_eq!(millis(Some("0000-01-01T00:00:00.000+01:00")), Null);
    assert_eq!(millis(Some("1970-01-01T00:00:00.0005Z")), Null);
    assert_eq!(millis(Some("1970-01-01T00:00:00Z")), Integer(0));
    assert_eq!(millis(None), Null);
}

#[test]
fn pending_messages_use_original_bytes_in_both_forms() {
    let mut rig = rig();
    let make = |id: &str, to: Option<&str>, status: MessageStatus, expires: Option<&str>| {
        let mut m = message(id);
        m.to_agent_id = to.map(str::to_owned);
        m.status = status;
        m.expires_at = expires.map(str::to_owned);
        m
    };
    let long_to = "r".repeat(65_537);
    let long_now = format!("2026-{}", "9".repeat(65_540));
    let rows = [
        make(
            "equal",
            Some("a"),
            MessageStatus::Pending,
            Some("2026-11-20T10:00:00Z"),
        ),
        make(
            "later",
            Some("a"),
            MessageStatus::Pending,
            Some("2026-11-20T10:00:01Z"),
        ),
        make(
            "earlier",
            Some("a"),
            MessageStatus::Pending,
            Some("2026-11-20T09:59:59Z"),
        ),
        make("never", Some("a"), MessageStatus::Pending, None),
        make("text", Some("a"), MessageStatus::Pending, Some("zzz")),
        make("empty-expiry", Some("a"), MessageStatus::Pending, Some("")),
        make("delivered", Some("a"), MessageStatus::Delivered, None),
        make("expired", Some("a"), MessageStatus::Expired, None),
        make("broadcast", None, MessageStatus::Pending, None),
        make("empty-to", Some(""), MessageStatus::Pending, None),
        make("case", Some("A"), MessageStatus::Pending, None),
        make("prefix", Some("ab"), MessageStatus::Pending, None),
        make("long-to", Some(&long_to), MessageStatus::Pending, None),
        make(
            "long-expiry",
            Some("a"),
            MessageStatus::Pending,
            Some(&long_now),
        ),
    ];
    for m in &rows {
        rig.create("p", m);
    }
    rig.create("q", &rows[3]);
    let now = "2026-11-20T10:00:00Z";
    let ids = |rig: &Rig, f: Filter| rig.ids(&f, Sort::Default, None, 100);
    // Equal expiry is selected, an absent one too, and opaque text compares as bytes.
    let (got, total, more) = ids(&rig, pending("a", now));
    assert_eq!((total, more), (got.len() as u64, false));
    let set = |v: Vec<String>| v.into_iter().collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        set(got),
        set(["equal", "later", "never", "text", "long-expiry"]
            .map(String::from)
            .to_vec())
    );
    // An empty expiry is below every non-empty `now` and equals only an empty one.
    assert!(set(ids(&rig, pending("a", "")).0).contains("empty-expiry"));
    assert!(!set(ids(&rig, pending("a", now)).0).contains("empty-expiry"));
    // Broadcast and absent recipients are never selected; empty equals only empty.
    assert_eq!(ids(&rig, pending("", now)).0, ["empty-to"]);
    assert_eq!(ids(&rig, pending("A", now)).0, ["case"]);
    assert_eq!(ids(&rig, pending("ab", now)).0, ["prefix"]);
    assert_eq!(ids(&rig, pending("a\u{0}", now)).0, Vec::<String>::new());
    // A recipient over 65,536 bytes is found by its digest and compared whole.
    assert_eq!(ids(&rig, pending(&long_to, now)).0, ["long-to"]);
    let near = format!("{}x", &long_to[1..]);
    assert!(ids(&rig, pending(&near, now)).0.is_empty());
    // A `now` over 65,536 bytes is compared whole against stored expiries of both forms.
    let (got, ..) = ids(&rig, pending("a", &long_now));
    assert!(set(got.clone()).contains("long-expiry"), "{got:?}");
    let past = format!("2026-{}", "9".repeat(65_541));
    assert!(!set(ids(&rig, pending("a", &past)).0).contains("long-expiry"));
    // Messages{recipient} is any status, the absent recipient is everyone.
    let (all, total, _) = ids(&rig, Filter::Messages { recipient: None });
    assert_eq!((all.len() as u64, total), (14, 14));
    assert_eq!(
        ids(
            &rig,
            Filter::Messages {
                recipient: Some("ab".into())
            }
        )
        .0,
        ["prefix"]
    );
    // Another project sees only its own.
    let page = rig
        .reader("q")
        .select(
            MessageSelection {
                filter: &pending("a", now),
                sort: Sort::Default,
            },
            None,
            10,
        )
        .unwrap();
    assert_eq!(page.items.iter().map(id_of).collect::<Vec<_>>(), ["never"]);
}
