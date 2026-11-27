use super::super::{
    cctg,
    entity_router::{EntityRead, EntityWrite, Family, Payload},
};
use super::*;
use rusqlite::Transaction;

fn family(kind: EntityKind) -> Option<Family> {
    Some(match kind {
        EntityKind::Task => Family::Task,
        EntityKind::Agent => Family::Agent,
        EntityKind::Team => Family::Team,
        EntityKind::Run => Family::Run,
        EntityKind::Message => Family::Message,
        EntityKind::Context | EntityKind::Config | EntityKind::Template | EntityKind::Goal => {
            return None;
        }
    })
}
fn native_family(kind: EntityKind) -> cctg::Family {
    match kind {
        EntityKind::Context => cctg::Family::Context,
        EntityKind::Config => cctg::Family::Config,
        EntityKind::Template => cctg::Family::Template,
        EntityKind::Goal => cctg::Family::Goal,
        EntityKind::Task
        | EntityKind::Agent
        | EntityKind::Team
        | EntityKind::Run
        | EntityKind::Message => unreachable!("router kind handled before native dispatch"),
    }
}
enum RoutedEntity {
    Router(Payload),
    Native(Entity),
}
fn route(entity: Entity) -> RoutedEntity {
    RoutedEntity::Router(match entity {
        Entity::Task(v) => Payload::Task(v),
        Entity::Agent(v) => Payload::Agent(v),
        Entity::Team(v) => Payload::Team(v),
        Entity::Run(v) => Payload::Run(v),
        Entity::Message(v) => Payload::Message(v),
        other => return RoutedEntity::Native(other),
    })
}

pub(super) struct ReadView<'a, 'conn> {
    pub(super) router: EntityRead<'a, 'conn>,
    pub(super) tx: &'a Transaction<'conn>,
    caller: &'a Caller,
    pub(super) shared: &'a Shared,
}
impl<'a, 'conn> ReadView<'a, 'conn> {
    pub(super) fn new(tx: &'a Transaction<'conn>, caller: &'a Caller, shared: &'a Shared) -> Self {
        Self {
            router: EntityRead::reader(tx, caller, &shared.disk, shared.state.clone()),
            tx,
            caller,
            shared,
        }
    }
    pub(super) fn observed<T>(&self, result: Result<T>) -> Result<T> {
        if let Err(e) = &result {
            self.shared.state.observe(FailureSource::Reader, e);
        }
        result
    }
    pub(super) fn get(&self, kind: EntityKind, id: &str) -> Result<Option<Record>> {
        if let Some(f) = family(kind) {
            self.router.get(f, id)
        } else {
            cctg::NativeSql::new(
                self.tx,
                &self.shared.disk,
                &self.caller.project,
                None,
                self.shared.clock.now_ms(),
            )
            .get(native_family(kind), id)
            .map_err(Into::into)
        }
    }
    pub(super) fn list(&self, f: &Filter, p: &PageRequest) -> Result<Page<Record>> {
        if matches!(f, Filter::Tasks { .. }) {
            self.router
                .task_page(&self.shared.task_cursors, self.shared.clock.now_ms(), f, p)
        } else {
            page::page(
                self.tx,
                &self.shared.disk,
                &self.caller.project,
                &self.shared.cursors,
                self.shared.clock.now_ms(),
                f,
                p,
            )
        }
    }
}

pub(super) struct Unit<'a, 'conn> {
    pub(super) router: EntityWrite<'a, 'conn>,
    native: cctg::NativeSql<'a, 'conn>,
    tx: &'a Transaction<'conn>,
    caller: &'a Caller,
    shared: &'a Shared,
    pub(super) now: i64,
}
impl<'a, 'conn> Unit<'a, 'conn> {
    pub(super) fn new(tx: &'a Transaction<'conn>, caller: &'a Caller, shared: &'a Shared) -> Self {
        let now = shared.clock.now_ms();
        Self {
            router: EntityWrite::new(tx, caller, &shared.disk, shared.state.clone(), now),
            native: cctg::NativeSql::new(
                tx,
                &shared.disk,
                &caller.project,
                Some(&caller.principal),
                now,
            ),
            tx,
            caller,
            shared,
            now,
        }
    }
    pub(super) fn observed<T>(&self, result: Result<T>) -> store::Result<T> {
        if let Err(error) = &result {
            self.shared.state.observe(FailureSource::Writer, error);
        }
        port(result)
    }
    pub(super) fn ensure_committable(&self) -> Result<()> {
        self.router.ensure_committable()?;
        self.native.ensure_committable().map_err(Into::into)
    }
}
impl store::UnitOfWork for Unit<'_, '_> {
    fn caller(&self) -> &Caller {
        self.caller
    }
    fn get(&self, kind: EntityKind, id: &str) -> store::Result<Option<Record>> {
        let result = (|| {
            self.ensure_committable()?;
            if let Some(f) = family(kind) {
                self.router.get(f, id)
            } else {
                self.native.get(native_family(kind), id).map_err(Into::into)
            }
        })();
        self.observed(result)
    }
    fn list(&self, f: &Filter, p: &PageRequest) -> store::Result<Page<Record>> {
        let result = (|| {
            self.ensure_committable()?;
            if matches!(f, Filter::Tasks { .. }) {
                self.router
                    .task_page(&self.shared.task_cursors, self.shared.clock.now_ms(), f, p)
            } else {
                page::page(
                    self.tx,
                    &self.shared.disk,
                    &self.caller.project,
                    &self.shared.cursors,
                    self.shared.clock.now_ms(),
                    f,
                    p,
                )
            }
        })();
        self.observed(result)
    }
    fn create(&mut self, entity: Entity) -> store::Result<Record> {
        let result = self
            .ensure_committable()
            .and_then(|()| match route(entity) {
                RoutedEntity::Router(p) => self.router.create(p),
                RoutedEntity::Native(e) => self.native.create(e).map_err(Into::into),
            });
        self.observed(result)
    }
    fn update(&mut self, entity: Entity, expected: Revision) -> store::Result<Record> {
        let result = self
            .ensure_committable()
            .and_then(|()| match route(entity) {
                RoutedEntity::Router(p) => self.router.update(p, expected),
                RoutedEntity::Native(e) => self.native.update(e, expected).map_err(Into::into),
            });
        self.observed(result)
    }
    fn delete(&mut self, kind: EntityKind, id: &str, expected: Revision) -> store::Result<()> {
        let result = self.ensure_committable().and_then(|()| {
            if let Some(f) = family(kind) {
                self.router.delete(f, id, expected)
            } else {
                self.native
                    .delete(native_family(kind), id, expected)
                    .map_err(Into::into)
            }
        });
        self.observed(result)
    }
    fn move_to_end(
        &mut self,
        kind: EntityKind,
        id: &str,
        expected: Revision,
    ) -> store::Result<Record> {
        let result = self.ensure_committable().and_then(|()| {
            if let Some(f) = family(kind) {
                self.router.move_to_end(f, id, expected)
            } else {
                self.native
                    .move_to_end(native_family(kind), id, expected)
                    .map_err(Into::into)
            }
        });
        self.observed(result)
    }
}
#[async_trait]
impl ReadSnapshot for Snapshot {
    fn caller(&self) -> &Caller {
        &self.caller
    }
    async fn get(&self, kind: EntityKind, id: &str) -> store::Result<Option<Record>> {
        let id = id.to_owned();
        self.call(move |view| view.get(kind, &id)).await
    }
    async fn list(&self, f: &Filter, p: &PageRequest) -> store::Result<Page<Record>> {
        let f = f.clone();
        let p = p.clone();
        self.call(move |view| view.list(&f, &p)).await
    }
}
