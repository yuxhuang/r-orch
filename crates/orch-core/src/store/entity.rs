//! Typed entity payloads and metadata; no project key can be embedded in a row.

use std::num::NonZeroU64;

use orch_domain::{Agent, Goal, Message, Run, Task, Team};
use serde::{Deserialize, Serialize};

use crate::storage::ContextEntry;

/// A positive entity version. Deserialization refuses zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Revision(NonZeroU64);

impl Revision {
    pub const ONE: Self = Self(NonZeroU64::MIN);

    pub fn get(self) -> u64 {
        self.0.get()
    }

    pub fn next(self) -> super::Result<Self> {
        self.get()
            .checked_add(1)
            .and_then(NonZeroU64::new)
            .map(Self)
            .ok_or(super::StoreError::CounterExhausted)
    }
}

/// Entity families of port v2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntityKind {
    Task,
    Goal,
    Agent,
    Team,
    Run,
    Message,
    Context,
    Config,
    Template,
}

/// One project setting (`project_config`). The key is the row's id; the value
/// is text that the engine interprets and the port never parses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigEntry {
    pub key: String,
    pub value: String,
}

/// One template (`templates`). The name is the row's id and is only ever
/// compared, never used as a path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemplateEntry {
    pub name: String,
    pub body: String,
}

/// Payload validation stays in the domain and the operation dispatcher.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum Entity {
    Task(Box<Task>),
    Goal(Box<Goal>),
    Agent(Box<Agent>),
    Team(Box<Team>),
    Run(Box<Run>),
    Message(Box<Message>),
    Context(ContextEntry),
    Config(ConfigEntry),
    Template(TemplateEntry),
}

impl Entity {
    pub fn kind(&self) -> EntityKind {
        match self {
            Self::Task(_) => EntityKind::Task,
            Self::Goal(_) => EntityKind::Goal,
            Self::Agent(_) => EntityKind::Agent,
            Self::Team(_) => EntityKind::Team,
            Self::Run(_) => EntityKind::Run,
            Self::Message(_) => EntityKind::Message,
            Self::Context(_) => EntityKind::Context,
            Self::Config(_) => EntityKind::Config,
            Self::Template(_) => EntityKind::Template,
        }
    }

    /// Exact bytes, including case, are used for every lookup.
    pub fn id(&self) -> &str {
        match self {
            Self::Task(v) => &v.id,
            Self::Goal(v) => &v.id,
            Self::Agent(v) => &v.id,
            Self::Team(v) => &v.id,
            Self::Run(v) => &v.id,
            Self::Message(v) => &v.id,
            Self::Context(v) => &v.key,
            Self::Config(v) => &v.key,
            Self::Template(v) => &v.name,
        }
    }
}

/// Position is monotonic within one project and entity family, even on delete.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    pub entity: Entity,
    pub revision: Revision,
    pub position: u64,
}
