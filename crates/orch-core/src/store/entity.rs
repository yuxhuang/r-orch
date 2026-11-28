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
    /// Refuse caller values outside the shared entity representation contract.
    /// This checks input only; persisted-data decoding has a distinct error path.
    pub fn validate_storage_input(&self) -> super::Result<()> {
        if let Self::Task(task) = self {
            for key in [
                Some(task.id.as_str()),
                task.assignee.as_deref(),
                task.goal_id.as_deref(),
                Some(task.created_at.as_str()),
                Some(task.updated_at.as_str()),
            ]
            .into_iter()
            .flatten()
            {
                if key.len() > 65_536 {
                    return Err(super::StoreError::Unrepresentable);
                }
            }
            for text in [&task.created_at, &task.updated_at] {
                // Reuse the core clock codec: round-trip equality rejects rolled
                // dates and alternate spellings. The Task format keeps the staged
                // SQLite range, whose upper bound precedes the end of year 9999.
                let valid = text.len() == 24
                    && crate::clock::epoch_ms_from_iso(text).is_some_and(|ms| {
                        (-62_167_219_200_000..=253_402_207_200_999).contains(&ms)
                            && crate::clock::iso_from_epoch_ms(ms) == *text
                    });
                if !valid {
                    return Err(super::StoreError::Unrepresentable);
                }
            }
        }
        Ok(())
    }

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

#[cfg(all(test, feature = "test-support"))]
mod permanent_input_tests {
    use super::*;
    use crate::store::{Caller, Principal, StoreError};
    use crate::test_support::make_task;

    #[test]
    fn task_keys_and_canonical_timestamps_have_exact_boundaries() {
        let mut task = make_task();
        task.created_at = "2026-01-01T00:00:00.000Z".into();
        task.updated_at = task.created_at.clone();
        task.id = "é".repeat(32768);
        task.assignee = Some("a".repeat(65536));
        task.goal_id = Some("g".repeat(65536));
        let valid = Entity::Task(Box::new(task.clone()));
        valid.validate_storage_input().unwrap();
        for field in ["id", "assignee", "goal"] {
            let mut refused = task.clone();
            match field {
                "id" => refused.id.push('a'),
                "assignee" => refused.assignee.as_mut().unwrap().push('a'),
                _ => refused.goal_id.as_mut().unwrap().push('a'),
            }
            assert!(matches!(
                Entity::Task(Box::new(refused)).validate_storage_input(),
                Err(StoreError::Unrepresentable)
            ));
        }
        for time in [
            "0000-01-01T00:00:00.000Z",
            "9999-12-30T22:00:00.999Z",
            "2024-02-29T00:00:00.000Z",
        ] {
            task.created_at = time.into();
            Entity::Task(Box::new(task.clone()))
                .validate_storage_input()
                .unwrap();
        }
        for time in [
            "9999-12-30T22:00:01.000Z",
            "9999-12-31T23:59:59.999Z",
            "2026-02-30T00:00:00.000Z",
            "2026-01-01T00:00:00Z",
            "2026-01-01T00:00:00.000+00:00",
            "+002026-01-01T00:00:00.000Z",
            "secret",
        ] {
            task.created_at = time.into();
            let error = Entity::Task(Box::new(task.clone()))
                .validate_storage_input()
                .unwrap_err();
            assert!(matches!(error, StoreError::Unrepresentable));
            assert_eq!(error.to_string(), "input cannot be represented by storage");
            assert_eq!(format!("{error:?}"), "Unrepresentable");
        }
    }

    #[test]
    fn context_provenance_bounds_only_persisted_identity_and_run() {
        let mut caller = Caller {
            project: "p".into(),
            principal: Principal::Task {
                identity: "i".repeat(65536),
                task: "t".repeat(65537),
                run: "r".repeat(65536),
                lineage: "l".into(),
                epoch: 1,
            },
        };
        caller.validate_context_provenance().unwrap();
        if let Principal::Task { run, .. } = &mut caller.principal {
            run.push('r');
        }
        assert!(matches!(
            caller.validate_context_provenance(),
            Err(StoreError::Unrepresentable)
        ));
        for principal in [
            Principal::Owner {
                identity: "i".repeat(65537),
            },
            Principal::Worker {
                identity: "i".repeat(65537),
            },
        ] {
            caller.principal = principal;
            assert!(matches!(
                caller.validate_context_provenance(),
                Err(StoreError::Unrepresentable)
            ));
        }
    }
}
