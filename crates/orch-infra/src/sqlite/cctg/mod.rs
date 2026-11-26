//! Context, Config, Template and Goal in Entity format v1: the byte detail and
//! the private native persistence of "Entity format v1 (SQLite)". Every SQL
//! operation borrows the selecting snapshot or the authorized caller's
//! transaction and never opens or commits one. Authorization, the public cursor
//! and R140 observation belong to the combined adapter, not to this leaf.
//!
//! One driver serves four flat families: each is a JSON object of strings (and
//! at most one number or enum word), so the family tables below hold the whole
//! difference between them. The strict reader, the text rule and the chunk
//! manifest are the Task format's own and are reused, not copied.
use super::task_format::{
    codec::{Detail, UniqueValue},
    text::{self, Chunk, Reference, Tag, TextError},
};
use crate::blob::{
    BlobError, BlobHash, BlobReferences, DiskBlobStore, INLINE_TEXT_MAX_BYTES, MAX_BLOB_BYTES,
    ReferenceError,
};
use orch_core::storage::ContextEntry;
use orch_core::store::{
    ConfigEntry, Entity, EntityKind, Filter, Principal, Record, Revision, Sort, StoreError,
    TemplateEntry,
};
use orch_domain::{Goal, GoalStatus};
use rusqlite::{OptionalExtension, Transaction, params, types::Value as SqlValue};
use serde_json::{Map, Value};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, thiserror::Error)]
pub(super) enum FormatError {
    #[error("stored entity detail failed {check} at {locator}")]
    Invalid {
        check: &'static str,
        locator: String,
    },
    #[error("entity object access failed at {locator}: {kind:?}")]
    Access {
        kind: std::io::ErrorKind,
        locator: String,
    },
    #[error(transparent)]
    Publish(#[from] TextError),
}
type Result<T> = std::result::Result<T, FormatError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Family {
    Context,
    Config,
    Template,
    Goal,
}

/// A text of the family with a native copy: the entity's JSON member, and the
/// column's quartet (`<c>`, `<c>_form`, `<c>_sha256`, `<c>_bytes`). The first
/// text of a family is its identity, which holds the digest in form 1.
struct Text {
    column: &'static str,
    member: &'static str,
}

#[derive(Clone, Copy)]
enum Scalar {
    /// Original string to nullable milliseconds, only when it is exactly one.
    Time(&'static str, &'static str),
    /// Exact unsigned member, NULL when absent or above `i64::MAX`.
    Unsigned(&'static str, &'static str),
    /// A domain word.
    Word(&'static str, &'static str),
}
impl Scalar {
    fn column(self) -> &'static str {
        match self {
            Self::Time(column, _) | Self::Unsigned(column, _) | Self::Word(column, _) => column,
        }
    }
}

const fn text(column: &'static str, member: &'static str) -> Text {
    Text { column, member }
}

impl Family {
    fn name(self) -> &'static str {
        match self {
            Self::Context => "context",
            Self::Config => "config",
            Self::Template => "template",
            Self::Goal => "goal",
        }
    }
    fn table(self) -> &'static str {
        match self {
            Self::Context => "context_entries",
            Self::Config => "project_config",
            Self::Template => "templates",
            Self::Goal => "goals",
        }
    }
    fn counter(self) -> &'static str {
        match self {
            Self::Context => "position:context",
            Self::Config => "position:config",
            Self::Template => "position:template",
            Self::Goal => "position:goal",
        }
    }
    fn kind(self) -> EntityKind {
        match self {
            Self::Context => EntityKind::Context,
            Self::Config => EntityKind::Config,
            Self::Template => EntityKind::Template,
            Self::Goal => EntityKind::Goal,
        }
    }
    fn of(entity: &Entity) -> NativeResult<Self> {
        match entity {
            Entity::Context(_) => Ok(Self::Context),
            Entity::Config(_) => Ok(Self::Config),
            Entity::Template(_) => Ok(Self::Template),
            Entity::Goal(_) => Ok(Self::Goal),
            _ => Err(NativeError::Encoding),
        }
    }
    fn texts(self) -> &'static [Text] {
        const CONTEXT: [Text; 2] = [text("key", "key"), text("value", "value")];
        const CONFIG: [Text; 2] = CONTEXT;
        const TEMPLATE: [Text; 2] = [text("name", "name"), text("body", "body")];
        const GOAL: [Text; 6] = [
            text("id", "id"),
            text("title", "title"),
            text("description", "description"),
            text("assignee_agent_id", "assignee"),
            text("created_at_text", "created_at"),
            text("updated_at_text", "updated_at"),
        ];
        match self {
            Self::Context => &CONTEXT,
            Self::Config => &CONFIG,
            Self::Template => &TEMPLATE,
            Self::Goal => &GOAL,
        }
    }
    fn scalars(self) -> &'static [Scalar] {
        match self {
            Self::Context => &[
                Scalar::Time("created_at", "created_at"),
                Scalar::Time("updated_at", "updated_at"),
                Scalar::Time("expires_at", "expires_at"),
                Scalar::Unsigned("ttl_ms", "ttl_ms"),
            ],
            Self::Config | Self::Template => &[],
            Self::Goal => &[
                Scalar::Word("status", "status"),
                Scalar::Time("created_at", "created_at"),
                Scalar::Time("updated_at", "updated_at"),
            ],
        }
    }
    /// Required and optional members of the detail root. Anything else, and a
    /// null, is refused before the domain's permissive serde sees it.
    fn members(self) -> (&'static [&'static str], &'static [&'static str]) {
        match self {
            Self::Context => (
                &["key", "value", "created_at", "updated_at"],
                &["ttl_ms", "expires_at"],
            ),
            Self::Config => (&["key", "value"], &[]),
            Self::Template => (&["name", "body"], &[]),
            Self::Goal => (
                &["id", "title", "description", "status", "created_at"],
                &["assignee", "updated_at"],
            ),
        }
    }
    /// Members that are not text slots.
    fn plain(self) -> &'static [&'static str] {
        match self {
            Self::Context => &["ttl_ms"],
            Self::Goal => &["status"],
            Self::Config | Self::Template => &[],
        }
    }
    fn identity(self) -> &'static str {
        self.texts()[0].column
    }
    fn columns(self) -> Vec<String> {
        let mut columns = Vec::new();
        for text in self.texts() {
            columns.extend([
                text.column.to_string(),
                format!("{}_form", text.column),
                format!("{}_sha256", text.column),
                format!("{}_bytes", text.column),
            ]);
        }
        columns.extend(self.scalars().iter().map(|s| s.column().to_string()));
        columns
    }
}

fn invalid(check: &'static str, locator: &str) -> FormatError {
    FormatError::Invalid {
        check,
        locator: locator.into(),
    }
}

/// The typed value as the detail's JSON, in the domain's field order.
fn payload(entity: &Entity) -> Result<Value> {
    match entity {
        Entity::Context(v) => serde_json::to_value(v),
        Entity::Config(v) => serde_json::to_value(v),
        Entity::Template(v) => serde_json::to_value(v),
        Entity::Goal(v) => serde_json::to_value(v),
        _ => return Err(invalid("family", "")),
    }
    .map_err(|_| invalid("typed encoding", ""))
}

fn typed(family: Family, value: Value) -> Result<Entity> {
    let entity = match family {
        Family::Context => serde_json::from_value::<ContextEntry>(value).map(Entity::Context),
        Family::Config => serde_json::from_value::<ConfigEntry>(value).map(Entity::Config),
        Family::Template => serde_json::from_value::<TemplateEntry>(value).map(Entity::Template),
        Family::Goal => serde_json::from_value::<Goal>(value).map(|v| Entity::Goal(Box::new(v))),
    };
    entity.map_err(|_| invalid("typed detail", ""))
}

/// Publish every oversized text and the aggregate through the blob adapter.
/// Nothing here installs a row: the caller's savepoint owns reachability.
pub(super) fn encode(store: &DiskBlobStore, entity: &Entity) -> Result<Detail> {
    let family = Family::of(entity).map_err(|_| invalid("family", ""))?;
    let mut value = payload(entity)?;
    let mut refs = BTreeMap::new();
    for locator in slots(family, &value)? {
        let slot = value
            .pointer_mut(&locator)
            .ok_or_else(|| invalid("text slot", &locator))?;
        let bytes = slot
            .as_str()
            .ok_or_else(|| invalid("text type", &locator))?
            .as_bytes();
        if bytes.len() > INLINE_TEXT_MAX_BYTES {
            let (reference, chunks) = text::publish(store, bytes)?;
            *slot = serde_json::to_value(Tag { reference })
                .map_err(|_| invalid("tag encoding", &locator))?;
            refs.insert(locator, chunks);
        }
    }
    let bytes = serde_json::to_vec(&value).map_err(|_| invalid("detail encoding", ""))?;
    let sha256 = BlobHash::of(&bytes).to_string();
    let inline = if bytes.len() <= INLINE_TEXT_MAX_BYTES {
        Some(String::from_utf8(bytes.clone()).map_err(|_| invalid("detail UTF-8", ""))?)
    } else {
        let (_, chunks) = text::publish(store, &bytes)?;
        refs.insert(String::new(), chunks);
        None
    };
    Ok(Detail {
        inline,
        bytes: bytes.len() as u64,
        sha256,
        refs,
    })
}

/// The SQL caller checks format, native projections, metadata and project-scoped
/// blob registration in its selecting snapshot. All locators here are schema-owned.
pub(super) fn decode(store: &DiskBlobStore, family: Family, detail: &Detail) -> Result<Entity> {
    let bytes = match &detail.inline {
        Some(text) if text.len() <= INLINE_TEXT_MAX_BYTES && text.len() as u64 == detail.bytes => {
            text.as_bytes().to_vec()
        }
        Some(_) => return Err(invalid("inline detail size", "")),
        None => read(
            store,
            &Reference {
                sha256: detail.sha256.clone(),
                bytes: detail.bytes,
                chunks: detail.bytes.div_ceil(MAX_BLOB_BYTES),
            },
            detail.refs.get("").map(Vec::as_slice).unwrap_or_default(),
            "",
        )?,
    };
    if bytes.len() as u64 != detail.bytes || BlobHash::of(&bytes).to_string() != detail.sha256 {
        return Err(invalid("detail length or hash", ""));
    }
    let mut value = serde_json::from_slice::<UniqueValue>(&bytes)
        .map_err(|_| invalid("strict JSON", ""))?
        .0;
    let mut used = BTreeSet::new();
    if detail.inline.is_none() {
        used.insert(String::new());
    }
    let mut tags = Vec::new();
    for locator in slots(family, &value)? {
        let slot = value
            .pointer(&locator)
            .ok_or_else(|| invalid("text slot", &locator))?;
        if let Some(text) = slot.as_str() {
            if text.len() > INLINE_TEXT_MAX_BYTES {
                return Err(invalid("inline text size", &locator));
            }
        } else {
            let tag: Tag = serde_json::from_value(slot.clone())
                .map_err(|_| invalid("reference tag", &locator))?;
            text::validate_chunks(
                &tag.reference,
                detail
                    .refs
                    .get(&locator)
                    .map(Vec::as_slice)
                    .unwrap_or_default(),
            )
            .map_err(|_| invalid("reference metadata", &locator))?;
            used.insert(locator.clone());
            tags.push((locator, tag));
        }
    }
    if detail.refs.keys().cloned().collect::<BTreeSet<_>>() != used {
        return Err(invalid("reference locators", ""));
    }
    // Prove inline types before opening slot objects: a transient access error
    // at one locator must not mask corruption that the inline parts already show.
    let mut preflight = value.clone();
    for (locator, _) in &tags {
        *preflight
            .pointer_mut(locator)
            .ok_or_else(|| invalid("text slot", locator))? = Value::String(String::new());
    }
    typed(family, preflight)?;
    for (locator, tag) in tags {
        let bytes = read(
            store,
            &tag.reference,
            detail
                .refs
                .get(&locator)
                .map(Vec::as_slice)
                .unwrap_or_default(),
            &locator,
        )?;
        *value
            .pointer_mut(&locator)
            .ok_or_else(|| invalid("text slot", &locator))? =
            Value::String(String::from_utf8(bytes).map_err(|_| invalid("text UTF-8", &locator))?);
    }
    typed(family, value)
}

fn read(
    store: &DiskBlobStore,
    reference: &Reference,
    rows: &[Chunk],
    locator: &str,
) -> Result<Vec<u8>> {
    // Read errors may contain paths. Only fixed checks and schema-derived locators escape.
    text::reconstruct(store, reference, rows).map_err(|error| match error {
        TextError::Access(kind) => FormatError::Access {
            kind,
            locator: locator.into(),
        },
        _ => invalid("referenced bytes", locator),
    })
}

fn object<'a>(
    value: &'a Value,
    required: &[&str],
    optional: &[&str],
) -> Result<&'a Map<String, Value>> {
    let object = value
        .as_object()
        .ok_or_else(|| invalid("object type", ""))?;
    if required.iter().any(|key| !object.contains_key(*key))
        || object.iter().any(|(key, value)| {
            value.is_null()
                || (!required.contains(&key.as_str()) && !optional.contains(&key.as_str()))
        })
    {
        return Err(invalid("object fields", ""));
    }
    Ok(object)
}

/// The text slots of a detail root, in its member order.
fn slots(family: Family, value: &Value) -> Result<Vec<String>> {
    let (required, optional) = family.members();
    Ok(object(value, required, optional)?
        .keys()
        .filter(|key| !family.plain().contains(&key.as_str()))
        .map(|key| format!("/{key}"))
        .collect())
}

type NativeResult<T> = std::result::Result<T, NativeError>;

#[derive(Debug, thiserror::Error)]
pub(super) enum NativeError {
    #[error("stored {family} form {form} failed {check} at {locator}")]
    Corrupt {
        family: &'static str,
        form: i64,
        check: &'static str,
        locator: String,
    },
    #[error(transparent)]
    Sql(#[from] rusqlite::Error),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Counter(Box<super::SqliteError>),
    #[error("entity object access failed: {0:?}")]
    Access(std::io::ErrorKind),
    #[error(transparent)]
    Publication(BlobError),
    #[error("entity encoding is not representable")]
    Encoding,
    #[error("a context entry is created only by an authenticated caller")]
    Provenance,
    #[error("native transaction cannot commit after failed rollback")]
    Aborted,
    #[error("operation failed: {operation}; rollback failed: {rollback}")]
    Rollback {
        operation: Box<NativeError>,
        rollback: rusqlite::Error,
    },
}

impl From<NativeError> for super::SqliteError {
    fn from(error: NativeError) -> Self {
        match error {
            NativeError::Corrupt {
                family,
                form,
                check,
                locator,
            } => Self::EntityCorrupt {
                family,
                form,
                check,
                locator,
            },
            NativeError::Sql(error) => Self::Sql(error),
            NativeError::Store(error) => Self::Store(error),
            NativeError::Counter(error) => *error,
            NativeError::Access(kind) => Self::EntityAccess(kind),
            NativeError::Publication(error) => Self::EntityPublication(error),
            NativeError::Encoding => Self::Unrepresentable("entity encoding".into()),
            NativeError::Provenance => Self::Unrepresentable("context provenance".into()),
            NativeError::Aborted => Self::TransactionAborted,
            NativeError::Rollback {
                operation,
                rollback,
            } => Self::OperationRollback {
                operation: Box::new((*operation).into()),
                rollback,
            },
        }
    }
}

fn native_corrupt(family: Family, form: i64, check: &'static str, locator: &str) -> NativeError {
    NativeError::Corrupt {
        family: family.name(),
        form,
        check,
        locator: locator.into(),
    }
}
/// The key form and stored key of an identity. Form 1 stores the digest.
fn key(id: &str) -> (i64, String) {
    if id.len() <= INLINE_TEXT_MAX_BYTES {
        (0, id.into())
    } else {
        (1, BlobHash::of(id.as_bytes()).to_string())
    }
}
fn integer(value: u64) -> NativeResult<i64> {
    i64::try_from(value).map_err(|_| StoreError::CounterExhausted.into())
}
fn unsigned(value: u64) -> SqlValue {
    i64::try_from(value)
        .map(SqlValue::Integer)
        .unwrap_or(SqlValue::Null)
}
/// Milliseconds only when the string is an instant of whole milliseconds. The
/// original string stays authoritative and is never rewritten from this column.
fn time(value: &str) -> SqlValue {
    value
        .parse::<jiff::Timestamp>()
        .ok()
        .filter(|t| t.as_nanosecond() % 1_000_000 == 0)
        .map(|t| SqlValue::Integer(t.as_millisecond()))
        .unwrap_or(SqlValue::Null)
}
fn quartet(value: Option<&str>, identity: bool) -> Vec<SqlValue> {
    match value {
        Some(text) if text.len() > INLINE_TEXT_MAX_BYTES => {
            let hash = BlobHash::of(text.as_bytes()).to_string();
            vec![
                SqlValue::Text(if identity {
                    hash.clone()
                } else {
                    String::new()
                }),
                SqlValue::Integer(1),
                SqlValue::Text(hash),
                SqlValue::Integer(text.len() as i64),
            ]
        }
        other => vec![
            other
                .map(|v| SqlValue::Text(v.into()))
                .unwrap_or(SqlValue::Null),
            SqlValue::Integer(0),
            SqlValue::Null,
            SqlValue::Null,
        ],
    }
}

/// Every native projection, derived from the typed value alone and in the
/// order of `Family::columns`.
fn projections(entity: &Entity) -> NativeResult<Vec<SqlValue>> {
    let family = Family::of(entity)?;
    let value = payload(entity).map_err(|_| NativeError::Encoding)?;
    let mut values = Vec::new();
    for (index, text) in family.texts().iter().enumerate() {
        values.extend(quartet(
            value.get(text.member).and_then(Value::as_str),
            index == 0,
        ));
    }
    for scalar in family.scalars() {
        values.push(match *scalar {
            Scalar::Time(_, member) => value
                .get(member)
                .and_then(Value::as_str)
                .map(time)
                .unwrap_or(SqlValue::Null),
            Scalar::Unsigned(_, member) => value
                .get(member)
                .and_then(Value::as_u64)
                .map(unsigned)
                .unwrap_or(SqlValue::Null),
            Scalar::Word(_, member) => value
                .get(member)
                .and_then(Value::as_str)
                .map(|v| SqlValue::Text(v.into()))
                .ok_or(NativeError::Encoding)?,
        });
    }
    Ok(values)
}

fn format_error(family: Family, form: i64, error: FormatError, writing: bool) -> NativeError {
    match error {
        FormatError::Access { kind, .. } | FormatError::Publish(TextError::Access(kind)) => {
            NativeError::Access(kind)
        }
        FormatError::Publish(TextError::Publish(error)) if writing => {
            NativeError::Publication(error)
        }
        _ if writing => NativeError::Encoding,
        FormatError::Invalid { check, locator } => native_corrupt(family, form, check, &locator),
        _ => native_corrupt(family, form, "detail encoding", ""),
    }
}

/// The principal's own part of a context row. Provenance is not part of the
/// entity: it is bound once on create and never rewritten by an update or a move.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Provenance {
    pub kind: String,
    /// `None` is a legacy row with no recorded identity, which an export must
    /// keep apart from an identity that is the empty string.
    pub identity: Option<String>,
    /// Only an authoritative binding sets an agent. No principal carries one
    /// today, so ordinary creation leaves it absent.
    pub agent: Option<String>,
    pub run: Option<String>,
}

impl Provenance {
    fn of(principal: &Principal) -> Self {
        match principal {
            Principal::Owner { identity } => Self::new("owner", identity, None),
            Principal::Worker { identity } => Self::new("worker", identity, None),
            Principal::Task { identity, run, .. } => Self::new("task", identity, Some(run)),
        }
    }
    fn new(kind: &str, identity: &str, run: Option<&String>) -> Self {
        Self {
            kind: kind.into(),
            identity: Some(identity.into()),
            agent: None,
            run: run.cloned(),
        }
    }
}

/// A context row's provenance columns. A row always names its kind, and every
/// other column is text or absent: anything else is damage, not "no provenance".
fn decode_provenance(form: i64, row: &[SqlValue]) -> NativeResult<Provenance> {
    let damaged = |locator| native_corrupt(Family::Context, form, "provenance", locator);
    let text = |value: &SqlValue, locator| match value {
        SqlValue::Null => Ok(None),
        SqlValue::Text(text) => Ok(Some(text.clone())),
        _ => Err(damaged(locator)),
    };
    let [kind, identity, agent, run] = row else {
        return Err(damaged("columns"));
    };
    Ok(Provenance {
        kind: text(kind, "set_by_kind")?.ok_or_else(|| damaged("set_by_kind"))?,
        identity: text(identity, "set_by_identity")?,
        agent: text(agent, "set_by_agent_id")?,
        run: text(run, "set_by_run_id")?,
    })
}

/// Private query composition only; public cursor identity and validation remain
/// owned by the combined adapter. All strings are exact original UTF-8 bytes.
pub(super) struct NativeSelection<'a> {
    pub(super) filter: &'a Filter,
    pub(super) sort: Sort,
}
pub(super) struct NativePage {
    pub(super) items: Vec<Record>,
    pub(super) total: u64,
    pub(super) has_more: bool,
}
impl NativeSelection<'_> {
    fn family(&self) -> NativeResult<Family> {
        let family = match self.filter {
            Filter::Context => Family::Context,
            Filter::Config => Family::Config,
            Filter::Templates => Family::Template,
            Filter::Goals { .. } => Family::Goal,
            _ => return Err(StoreError::InvalidPage("unsupported native family".into()).into()),
        };
        if !(matches!(self.sort, Sort::Default | Sort::Id)
            || (self.sort == Sort::CreatedAt && family == Family::Goal))
        {
            return Err(StoreError::InvalidPage("unsupported native selection".into()).into());
        }
        Ok(family)
    }
    fn matches(&self, row: &Record) -> bool {
        match (&row.entity, self.filter) {
            (Entity::Goal(goal), Filter::Goals { status }) => {
                status.is_none_or(|status| status == goal.status)
            }
            (Entity::Context(_), Filter::Context)
            | (Entity::Config(_), Filter::Config)
            | (Entity::Template(_), Filter::Templates) => true,
            _ => false,
        }
    }
    /// The reference's total order, on original bytes. The family and sort are
    /// validated before any comparison, anchors included.
    fn compare(&self, a: &Record, b: &Record) -> Ordering {
        let primary = match (&a.entity, &b.entity, self.sort) {
            (_, _, Sort::Id) => Ordering::Equal,
            (Entity::Goal(a), Entity::Goal(b), _) if self.sort == Sort::CreatedAt => {
                a.created_at.as_bytes().cmp(b.created_at.as_bytes())
            }
            (Entity::Goal(a), Entity::Goal(b), _) => rank(a.status).cmp(&rank(b.status)).then(
                // Present before absent, an empty text included, newest text first.
                match (&a.updated_at, &b.updated_at) {
                    (Some(a), Some(b)) => b.as_bytes().cmp(a.as_bytes()),
                    (Some(_), None) => Ordering::Less,
                    (None, Some(_)) => Ordering::Greater,
                    (None, None) => Ordering::Equal,
                },
            ),
            _ => Ordering::Equal,
        };
        let primary = if matches!(self.filter, Filter::Goals { .. }) && self.sort != Sort::Id {
            primary.then(a.position.cmp(&b.position))
        } else {
            primary
        };
        primary.then_with(|| a.entity.id().as_bytes().cmp(b.entity.id().as_bytes()))
    }
}
fn rank(status: GoalStatus) -> usize {
    GoalStatus::ALL
        .iter()
        .position(|candidate| *candidate == status)
        .unwrap_or(usize::MAX)
}

/// Native persistence on a borrowed transaction. `principal` is the authorized
/// caller of the unit of work, or `None` on a read-only or goal-only path.
pub(super) struct NativeSql<'a, 'conn> {
    tx: &'a Transaction<'conn>,
    disk: &'a DiskBlobStore,
    project: &'a str,
    principal: Option<&'a Principal>,
    poisoned: bool,
    now: i64,
}

impl<'a, 'conn> NativeSql<'a, 'conn> {
    pub(super) fn new(
        tx: &'a Transaction<'conn>,
        disk: &'a DiskBlobStore,
        project: &'a str,
        principal: Option<&'a Principal>,
        now: i64,
    ) -> Self {
        Self {
            tx,
            disk,
            project,
            principal,
            poisoned: false,
            now,
        }
    }
    pub(super) fn ensure_committable(&self) -> NativeResult<()> {
        if self.poisoned || self.tx.is_autocommit() {
            Err(NativeError::Aborted)
        } else {
            Ok(())
        }
    }
    /// A caught operation error undoes its row, detail, references and counter,
    /// and keeps the unit's earlier successful work.
    fn atomic<T>(&mut self, work: impl FnOnce(&Self) -> NativeResult<T>) -> NativeResult<T> {
        self.ensure_committable()?;
        self.tx.execute_batch("SAVEPOINT cctg_entity")?;
        let result = work(self).and_then(|v| {
            self.tx.execute_batch("RELEASE cctg_entity")?;
            Ok(v)
        });
        match result {
            Ok(value) => Ok(value),
            // Some SQLite errors abort the whole transaction, savepoint included.
            // Keep both results: the caller needs them to classify the failure.
            Err(operation) => match self
                .tx
                .execute_batch("ROLLBACK TO cctg_entity; RELEASE cctg_entity")
            {
                Ok(()) => Err(operation),
                Err(rollback) => {
                    self.poisoned = true;
                    Err(NativeError::Rollback {
                        operation: Box::new(operation),
                        rollback,
                    })
                }
            },
        }
    }
    // Compiled identifiers only. Keys, locators and payloads are always bound.
    fn rows(&self, query: &str, values: &[SqlValue]) -> NativeResult<Vec<Vec<SqlValue>>> {
        let mut stmt = self.tx.prepare(query)?;
        let count = stmt.column_count();
        Ok(stmt
            .query_map(rusqlite::params_from_iter(values), |r| {
                (0..count).map(|i| r.get(i)).collect()
            })?
            .collect::<rusqlite::Result<_>>()?)
    }
    /// Execute one compiled statement with every value bound.
    fn run(&self, query: &str, values: &[SqlValue]) -> NativeResult<usize> {
        let mut stmt = self.tx.prepare(query)?;
        // `raw_execute` binds an unbound parameter as NULL, so the count that
        // `execute` would check is checked here.
        if stmt.parameter_count() != values.len() {
            return Err(rusqlite::Error::InvalidParameterCount(
                values.len(),
                stmt.parameter_count(),
            )
            .into());
        }
        for (i, value) in values.iter().enumerate() {
            stmt.raw_bind_parameter(i + 1, value)?;
        }
        Ok(stmt.raw_execute()?)
    }
    fn locator(&self, form: i64, stored: &str) -> Vec<SqlValue> {
        vec![
            SqlValue::Text(self.project.into()),
            SqlValue::Integer(form),
            SqlValue::Text(stored.into()),
        ]
    }
    fn identity(&self, id: &str) -> Vec<SqlValue> {
        let (form, stored) = key(id);
        self.locator(form, &stored)
    }

    pub(super) fn get(&self, family: Family, id: &str) -> NativeResult<Option<Record>> {
        self.ensure_committable()?;
        let (form, stored) = key(id);
        self.lookup(family, id, form, &stored)
    }

    /// The row stored under `form` and `stored`, if its original identifier is
    /// exactly `id`. A digest only locates the candidate: the requested bytes
    /// decide, so a different identifier that shares the locator is not found.
    fn lookup(
        &self,
        family: Family,
        id: &str,
        form: i64,
        stored: &str,
    ) -> NativeResult<Option<Record>> {
        Ok(self
            .row(family, form, stored)?
            .filter(|record| record.entity.id() == id))
    }

    /// Hydrate one row by its stored key, or `None` when no row has it.
    fn row(&self, family: Family, form: i64, stored: &str) -> NativeResult<Option<Record>> {
        self.row_inner(family, form, stored)
            .map_err(|error| match error {
                NativeError::Sql(rusqlite::Error::Utf8Error(..)) => {
                    native_corrupt(family, form, "text encoding", "native/detail/ref")
                }
                other => other,
            })
    }

    fn row_inner(&self, family: Family, form: i64, stored: &str) -> NativeResult<Option<Record>> {
        let columns = family.columns();
        let id = family.identity();
        let rows = self.rows(
            &format!(
                "SELECT {},revision,position FROM {} WHERE project_id=?1 AND {id}_form=?2 AND {id}=?3",
                columns.join(","),
                family.table()
            ),
            &self.locator(form, stored),
        )?;
        let Some(native) = rows.first() else {
            return Ok(None);
        };
        let n = columns.len();
        let (SqlValue::Integer(revision), SqlValue::Integer(position)) =
            (&native[n], &native[n + 1])
        else {
            return Err(native_corrupt(
                family,
                form,
                "counter type",
                "revision/position",
            ));
        };
        if *revision <= 0 || *position <= 0 {
            return Err(native_corrupt(family, form, "counter", "revision/position"));
        }
        let detail = self.detail(family, form, stored)?;
        let entity =
            decode(self.disk, family, &detail).map_err(|e| format_error(family, form, e, false))?;
        if key(entity.id()) != (form, stored.to_owned()) {
            return Err(native_corrupt(family, form, "identity projection", "id"));
        }
        let expected = projections(&entity)
            .map_err(|_| native_corrupt(family, form, "projection encoding", ""))?;
        for (i, column) in columns.iter().enumerate() {
            if native[i] != expected[i] {
                return Err(native_corrupt(family, form, "projection", column));
            }
        }
        let revision = serde_json::from_value((*revision).into())
            .map_err(|_| native_corrupt(family, form, "revision", "revision"))?;
        Ok(Some(Record {
            entity,
            revision,
            position: *position as u64,
        }))
    }

    /// Hydrate every candidate in this selecting transaction and compare the
    /// originals. No digest or index order can exclude a row whose original key
    /// takes part in the total order, and no record escapes if any candidate is
    /// corrupt. The composition owner binds the saved anchor to its project,
    /// filter and sort in the public cursor.
    pub(super) fn select(
        &self,
        query: NativeSelection<'_>,
        after: Option<&Record>,
        limit: usize,
    ) -> NativeResult<NativePage> {
        self.ensure_committable()?;
        let family = query.family()?;
        if !(1..=1000).contains(&limit) {
            return Err(StoreError::InvalidPage("limit must be 1..=1000".into()).into());
        }
        if let Some(anchor) = after
            && (Family::of(&anchor.entity)? != family || !query.matches(anchor))
        {
            return Err(StoreError::InvalidCursor.into());
        }
        let id = family.identity();
        let mut stmt = self.tx.prepare(&format!(
            "SELECT {id}_form,{id} FROM {} WHERE project_id=?1",
            family.table()
        ))?;
        // Keep the stored form even when decoding the key fails: malformed TEXT
        // is stored-data corruption before any detail is read.
        let keys = stmt
            .query_map([self.project], |row| {
                Ok((row.get::<_, SqlValue>(0)?, row.get::<_, SqlValue>(1)))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut records = Vec::new();
        for (form, stored) in keys {
            let SqlValue::Integer(form @ (0 | 1)) = form else {
                return Err(native_corrupt(family, -1, "identity metadata", "id"));
            };
            let stored = stored.map_err(|error| match error {
                rusqlite::Error::Utf8Error(..) => {
                    native_corrupt(family, form, "text encoding", "native/id")
                }
                other => NativeError::Sql(other),
            })?;
            let SqlValue::Text(stored) = stored else {
                return Err(native_corrupt(family, form, "identity metadata", "id"));
            };
            // A stored key is a locator, not an original ID: form 1's key can
            // equal a form 0 ID, so a digest is never fed back into `get`.
            let candidate = self
                .row(family, form, &stored)?
                .ok_or_else(|| native_corrupt(family, form, "selected row missing", "id"))?;
            if query.matches(&candidate) {
                records.push(candidate);
            }
        }
        records.sort_by(|a, b| query.compare(a, b));
        let total = records.len() as u64;
        let mut remaining = records.into_iter().filter(|row| {
            after.is_none_or(|anchor| query.compare(row, anchor) == Ordering::Greater)
        });
        let items = remaining.by_ref().take(limit).collect();
        let has_more = remaining.next().is_some();
        Ok(NativePage {
            items,
            total,
            has_more,
        })
    }

    pub(super) fn hydrate(&self, family: Family, ids: &[String]) -> NativeResult<Vec<Record>> {
        ids.iter()
            .map(|id| {
                self.get(family, id)?.ok_or_else(|| {
                    let (form, _) = key(id);
                    native_corrupt(family, form, "selected row missing", "id")
                })
            })
            .collect()
    }

    /// The caller's provenance as bound on create, for the export path and tests.
    pub(super) fn provenance(&self, id: &str) -> NativeResult<Option<Provenance>> {
        let (form, stored) = key(id);
        let rows = self.rows(
            "SELECT set_by_kind,set_by_identity,set_by_agent_id,set_by_run_id FROM context_entries
             WHERE project_id=?1 AND key_form=?2 AND key=?3",
            &self.locator(form, &stored),
        )?;
        rows.first()
            .map(|row| decode_provenance(form, row))
            .transpose()
    }

    fn detail(&self, family: Family, form: i64, stored: &str) -> NativeResult<Detail> {
        let mut identity = self.locator(form, stored);
        identity.push(SqlValue::Text(family.name().into()));
        let rows = self.rows("SELECT format,detail,detail_bytes,detail_sha256 FROM entity_details WHERE project_id=?1 AND key_form=?2 AND entity_key=?3 AND family=?4", &identity)?;
        let Some(row) = rows.first() else {
            return Err(native_corrupt(family, form, "detail missing", ""));
        };
        let [
            SqlValue::Integer(1),
            inline,
            SqlValue::Integer(bytes),
            SqlValue::Text(hash),
        ] = row.as_slice()
        else {
            return Err(native_corrupt(family, form, "detail metadata", ""));
        };
        if *bytes < 0 || hash.parse::<BlobHash>().is_err() {
            return Err(native_corrupt(family, form, "detail metadata", ""));
        }
        let inline = match inline {
            SqlValue::Null if *bytes > INLINE_TEXT_MAX_BYTES as i64 => None,
            SqlValue::Text(s) if s.len() <= INLINE_TEXT_MAX_BYTES && s.len() as i64 == *bytes => {
                Some(s.clone())
            }
            _ => return Err(native_corrupt(family, form, "detail size", "")),
        };
        let rows = self.rows("SELECT locator,ordinal,sha256,size FROM entity_blob_refs WHERE project_id=?1 AND key_form=?2 AND entity_key=?3 AND family=?4 ORDER BY locator COLLATE BINARY,ordinal", &identity)?;
        let mut refs: BTreeMap<String, Vec<Chunk>> = BTreeMap::new();
        for row in rows {
            let [
                SqlValue::Text(locator),
                SqlValue::Integer(ordinal),
                SqlValue::Text(hash),
                SqlValue::Integer(size),
            ] = row.as_slice()
            else {
                return Err(native_corrupt(family, form, "reference metadata", ""));
            };
            if *ordinal < 0
                || *size <= 0
                || *size as u64 > MAX_BLOB_BYTES
                || hash.parse::<BlobHash>().is_err()
            {
                return Err(native_corrupt(
                    family,
                    form,
                    "reference metadata",
                    "blob_ref",
                ));
            }
            let registration = self.rows(
                "SELECT size,storage_location FROM blobs WHERE project_id=?1 AND sha256=?2",
                &[
                    SqlValue::Text(self.project.into()),
                    SqlValue::Text(hash.clone()),
                ],
            )?;
            if registration
                != vec![vec![
                    SqlValue::Integer(*size),
                    SqlValue::Text("disk".into()),
                ]]
            {
                return Err(native_corrupt(
                    family,
                    form,
                    "blob registration",
                    "blob_ref",
                ));
            }
            refs.entry(locator.clone()).or_default().push(Chunk {
                ordinal: *ordinal as u64,
                sha256: hash.clone(),
                size: *size as u64,
            });
        }
        Ok(Detail {
            inline,
            bytes: *bytes as u64,
            sha256: hash.clone(),
            refs,
        })
    }

    fn current(&self, family: Family, id: &str, expected: Revision) -> NativeResult<Record> {
        match self.get(family, id)? {
            Some(record) if record.revision == expected => Ok(record),
            other => Err(StoreError::StaleRevision {
                kind: family.kind(),
                id: id.into(),
                expected,
                actual: other.map(|r| r.revision),
            }
            .into()),
        }
    }

    /// The next position from the durable counter, never from `MAX(position)`:
    /// a delete must not make an issued number available again. A counter that
    /// is missing or behind the rows is corruption, not permission to reuse one.
    fn next_position(&self, family: Family, id: &str) -> NativeResult<u64> {
        let counter: Option<i64> = self
            .tx
            .query_row(
                "SELECT value FROM counters WHERE project_id=?1 AND name=?2",
                params![self.project, family.counter()],
                |r| r.get(0),
            )
            .optional()?;
        let last = counter.unwrap_or(0);
        if last < 0 {
            return Err(NativeError::Counter(Box::new(
                super::SqliteError::CounterNegative {
                    project: self.project.into(),
                    name: family.counter(),
                    value: last,
                },
            )));
        }
        let (min, max): (Option<i64>, Option<i64>) = self.tx.query_row(
            &format!(
                "SELECT MIN(position),MAX(position) FROM {} WHERE project_id=?1",
                family.table()
            ),
            [self.project],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        if min.is_some_and(|v| v <= 0) {
            return Err(native_corrupt(family, key(id).0, "counter", "position"));
        }
        if max.is_some_and(|v| v > last) {
            return Err(NativeError::Counter(Box::new(if counter.is_none() {
                super::SqliteError::CounterMissing {
                    project: self.project.into(),
                    name: family.counter(),
                }
            } else {
                super::SqliteError::CounterBehind {
                    project: self.project.into(),
                    name: family.counter(),
                    value: last,
                }
            })));
        }
        Ok(last.checked_add(1).ok_or(StoreError::CounterExhausted)? as u64)
    }
    fn store_position(&self, family: Family, position: u64) -> NativeResult<()> {
        self.tx.execute("INSERT INTO counters(project_id,name,value) VALUES(?1,?2,?3) ON CONFLICT(project_id,name) DO UPDATE SET value=excluded.value", params![self.project, family.counter(), integer(position)?])?;
        Ok(())
    }
    fn encode(&self, entity: &Entity) -> NativeResult<Detail> {
        let family = Family::of(entity)?;
        encode(self.disk, entity).map_err(|e| format_error(family, key(entity.id()).0, e, true))
    }

    /// Bind provenance from the caller, refusing before any write.
    fn bound(&self) -> NativeResult<Provenance> {
        let provenance = self
            .principal
            .map(Provenance::of)
            .ok_or(NativeError::Provenance)?;
        let long = |text: &str| text.len() > INLINE_TEXT_MAX_BYTES;
        if provenance.identity.as_deref().is_some_and(long)
            || provenance.run.as_deref().is_some_and(long)
        {
            return Err(NativeError::Encoding);
        }
        Ok(provenance)
    }

    pub(super) fn create(&mut self, entity: Entity) -> NativeResult<Record> {
        self.ensure_committable()?;
        let family = Family::of(&entity)?;
        let id = entity.id();
        let (form, stored) = key(id);
        if self.get(family, id)?.is_some() {
            return Err(StoreError::AlreadyExists {
                kind: family.kind(),
                id: id.into(),
            }
            .into());
        }
        let mut identity = self.locator(form, &stored);
        identity.push(SqlValue::Text(family.name().into()));
        for table in ["entity_details", "entity_blob_refs"] {
            if !self.rows(&format!("SELECT 1 FROM {table} WHERE project_id=?1 AND key_form=?2 AND entity_key=?3 AND family=?4 LIMIT 1"), &identity)?.is_empty() {
                return Err(native_corrupt(family, form, "orphan row", table));
            }
        }
        let provenance = (family == Family::Context)
            .then(|| self.bound())
            .transpose()?;
        let position = self.next_position(family, id)?;
        let projected = projections(&entity)?;
        let detail = self.encode(&entity)?;
        self.atomic(|this| {
            let mut columns = family.columns();
            columns.extend(["project_id".into(), "revision".into(), "position".into()]);
            let mut values = projected;
            values.extend([
                SqlValue::Text(this.project.into()),
                SqlValue::Integer(1),
                SqlValue::Integer(integer(position)?),
            ]);
            if let Some(p) = provenance {
                columns.extend(
                    [
                        "set_by_kind",
                        "set_by_identity",
                        "set_by_agent_id",
                        "set_by_run_id",
                    ]
                    .map(String::from),
                );
                let text = |v: Option<String>| v.map(SqlValue::Text).unwrap_or(SqlValue::Null);
                values.extend([
                    SqlValue::Text(p.kind),
                    text(p.identity),
                    text(p.agent),
                    text(p.run),
                ]);
            }
            let slots = (1..=values.len())
                .map(|i| format!("?{i}"))
                .collect::<Vec<_>>()
                .join(",");
            this.run(
                &format!(
                    "INSERT INTO {}({}) VALUES({slots})",
                    family.table(),
                    columns.join(",")
                ),
                &values,
            )?;
            this.install(family, entity.id(), &detail)?;
            this.store_position(family, position)?;
            Ok(Record {
                entity,
                revision: Revision::ONE,
                position,
            })
        })
    }

    /// The entity's own projections, its detail and a checked revision change.
    /// Position, provenance and the Goal's integration and publication columns
    /// are not written: they belong to other owners.
    pub(super) fn update(&mut self, entity: Entity, expected: Revision) -> NativeResult<Record> {
        self.ensure_committable()?;
        let family = Family::of(&entity)?;
        let old = self.current(family, entity.id(), expected)?;
        let revision = expected.next()?;
        integer(revision.get())?;
        let projected = projections(&entity)?;
        let detail = self.encode(&entity)?;
        self.atomic(|this| {
            let mut values = this.identity(entity.id());
            values.extend(projected);
            values.push(SqlValue::Integer(integer(revision.get())?));
            let identity = family.identity();
            let mut assignments = family
                .columns()
                .iter()
                .enumerate()
                .map(|(i, c)| format!("{c}=?{}", i + 4))
                .collect::<Vec<_>>();
            assignments.push(format!("revision=?{}", values.len()));
            this.run(
                &format!(
                    "UPDATE {} SET {} WHERE project_id=?1 AND {identity}_form=?2 AND {identity}=?3",
                    family.table(),
                    assignments.join(",")
                ),
                &values,
            )?;
            this.install(family, entity.id(), &detail)?;
            Ok(Record {
                entity,
                revision,
                position: old.position,
            })
        })
    }

    pub(super) fn move_to_end(
        &mut self,
        family: Family,
        id: &str,
        expected: Revision,
    ) -> NativeResult<Record> {
        self.ensure_committable()?;
        let old = self.current(family, id, expected)?;
        let revision = expected.next()?;
        integer(revision.get())?;
        let position = self.next_position(family, id)?;
        self.atomic(|this| {
            let mut values = this.identity(id);
            values.extend([
                SqlValue::Integer(integer(revision.get())?),
                SqlValue::Integer(integer(position)?),
            ]);
            let identity = family.identity();
            this.run(
                &format!(
                    "UPDATE {} SET revision=?4,position=?5 WHERE project_id=?1 AND {identity}_form=?2 AND {identity}=?3",
                    family.table()
                ),
                &values,
            )?;
            this.store_position(family, position)?;
            Ok(Record {
                entity: old.entity,
                revision,
                position,
            })
        })
    }

    pub(super) fn delete(
        &mut self,
        family: Family,
        id: &str,
        expected: Revision,
    ) -> NativeResult<()> {
        self.ensure_committable()?;
        self.current(family, id, expected)?;
        self.atomic(|this| {
            this.clear(family, id)?;
            let identity = family.identity();
            this.run(
                &format!(
                    "DELETE FROM {} WHERE project_id=?1 AND {identity}_form=?2 AND {identity}=?3",
                    family.table()
                ),
                &this.identity(id),
            )?;
            Ok(())
        })
    }

    fn clear(&self, family: Family, id: &str) -> NativeResult<()> {
        let mut values = self.identity(id);
        values.push(SqlValue::Text(family.name().into()));
        for table in ["entity_details", "entity_blob_refs"] {
            self.run(&format!("DELETE FROM {table} WHERE project_id=?1 AND key_form=?2 AND entity_key=?3 AND family=?4"), &values)?;
        }
        Ok(())
    }

    /// Replace the detail and its reference rows; every object was published
    /// before this call, and each is registered for the project here.
    fn install(&self, family: Family, id: &str, detail: &Detail) -> NativeResult<()> {
        let (form, stored) = key(id);
        self.clear(family, id)?;
        self.tx.execute("INSERT INTO entity_details(project_id,family,key_form,entity_key,format,detail,detail_bytes,detail_sha256) VALUES(?1,?2,?3,?4,1,?5,?6,?7)", params![self.project, family.name(), form, stored, detail.inline, integer(detail.bytes)?, detail.sha256])?;
        for (locator, chunks) in &detail.refs {
            for chunk in chunks {
                let hash = chunk
                    .sha256
                    .parse::<BlobHash>()
                    .map_err(|_| NativeError::Encoding)?;
                BlobReferences::register_published(
                    self.tx,
                    self.project,
                    &hash,
                    integer(chunk.size)?,
                    self.now,
                )
                .map_err(|e| match e {
                    ReferenceError::Sql(e) => NativeError::Sql(e),
                    _ => native_corrupt(family, form, "blob registration", "blob_ref"),
                })?;
                self.tx.execute("INSERT INTO entity_blob_refs(project_id,family,key_form,entity_key,locator,ordinal,sha256,size) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)", params![self.project, family.name(), form, stored, locator, integer(chunk.ordinal)?, chunk.sha256, integer(chunk.size)?])?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod driver_regression;
#[cfg(test)]
mod tests;
