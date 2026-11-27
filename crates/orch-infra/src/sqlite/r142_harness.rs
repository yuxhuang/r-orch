//! Test-only borrowed bridge for the retained physical format corpus.
use super::agent_team_run::{Family as NativeFamily, NativeSql};
use super::entity_router::{EntityRead, EntityWrite, Family, Payload, native};
use super::{FailureSource, FailureState, Result};
use crate::blob::DiskBlobStore;
use orch_core::store::{Caller, Entity, EntityKind, Principal, Record, Revision};
use rusqlite::Transaction;
use std::sync::LazyLock;

static CALLER: LazyLock<Caller> = LazyLock::new(|| Caller {
    project: "p".into(),
    principal: Principal::Owner {
        identity: "synthetic".into(),
    },
});

pub(super) struct NativeFormatHarness<'a, 'conn> {
    writer: EntityWrite<'a, 'conn>,
    reader: EntityRead<'a, 'conn>,
    native: NativeSql<'a, 'conn>,
    state: FailureState,
    source: FailureSource,
}

impl<'a, 'conn> NativeFormatHarness<'a, 'conn> {
    pub(super) fn new(
        tx: &'a Transaction<'conn>,
        disk: &'a DiskBlobStore,
        project: &str,
        now: i64,
        state: FailureState,
        source: FailureSource,
    ) -> Self {
        assert_eq!(project, CALLER.project);
        Self {
            writer: EntityWrite::new(tx, &CALLER, disk, state.clone(), now),
            reader: EntityRead::reader(tx, &CALLER, disk, state.clone()),
            native: NativeSql::new(tx, disk, &CALLER.project, now),
            state,
            source,
        }
    }
    pub(super) fn ensure_committable(&self) -> Result<()> {
        self.writer.ensure_committable()
    }
    pub(super) fn get(&self, kind: EntityKind, id: &str) -> Result<Option<Record>> {
        if self.source == FailureSource::Writer {
            self.writer.get(family(kind), id)
        } else {
            self.reader.get(family(kind), id)
        }
    }
    pub(super) fn create(&mut self, entity: Entity) -> Result<Record> {
        assert_eq!(self.source, FailureSource::Writer);
        self.writer.create(payload(entity))
    }
    pub(super) fn update(&mut self, entity: Entity, expected: Revision) -> Result<Record> {
        assert_eq!(self.source, FailureSource::Writer);
        self.writer.update(payload(entity), expected)
    }
    pub(super) fn delete(&mut self, kind: EntityKind, id: &str, expected: Revision) -> Result<()> {
        assert_eq!(self.source, FailureSource::Writer);
        self.writer.delete(family(kind), id, expected)
    }
    pub(super) fn move_to_end(
        &mut self,
        kind: EntityKind,
        id: &str,
        expected: Revision,
    ) -> Result<Record> {
        assert_eq!(self.source, FailureSource::Writer);
        self.writer.move_to_end(family(kind), id, expected)
    }
    pub(super) fn hydrate(&self, kind: EntityKind, ids: &[String]) -> Result<Vec<Record>> {
        if self.source == FailureSource::Writer {
            self.ensure_committable()?;
        }
        let kind = match kind {
            EntityKind::Agent => NativeFamily::Agent,
            EntityKind::Team => NativeFamily::Team,
            EntityKind::Run => NativeFamily::Run,
            _ => unreachable!("physical corpus family"),
        };
        let result = self.native.hydrate(kind, ids).map_err(native);
        if let Err(error) = &result {
            self.state.observe(self.source, error);
        }
        result
    }
}
fn family(kind: EntityKind) -> Family {
    match kind {
        EntityKind::Agent => Family::Agent,
        EntityKind::Team => Family::Team,
        EntityKind::Run => Family::Run,
        _ => unreachable!("physical corpus family"),
    }
}
fn payload(entity: Entity) -> Payload {
    match entity {
        Entity::Agent(v) => Payload::Agent(v),
        Entity::Team(v) => Payload::Team(v),
        Entity::Run(v) => Payload::Run(v),
        _ => unreachable!("physical corpus family"),
    }
}
