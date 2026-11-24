//! Bounded lists. Cursors are backend-owned opaque tokens, never offsets.

use orch_domain::{GoalStatus, TaskStatus};
use serde::{Deserialize, Serialize};

use super::EntityKind;

/// Backend-specific text; consumers must return it unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Cursor(String);

impl Cursor {
    pub fn new(value: String) -> super::Result<Self> {
        if value.is_empty() || value.len() > 512 {
            return Err(super::StoreError::InvalidCursor);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Per-family sorts; backends refuse combinations that family does not offer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Sort {
    #[default]
    Default,
    Id,
    Position,
    Name,
    Priority,
    CreatedAt,
    UpdatedAt,
    /// Numeric attempt descending, then id; requires a task-scoped Runs filter.
    Attempt,
}

/// Typed filters cannot accidentally apply a task status to another family.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Filter {
    Tasks {
        status: Option<TaskStatus>,
        goal: Option<String>,
    },
    Goals {
        status: Option<GoalStatus>,
    },
    Agents,
    Teams,
    Runs {
        task: Option<String>,
        agent: Option<String>,
    },
    Messages {
        recipient: Option<String>,
    },
    /// Pending for one exact recipient, at a caller-captured lexical time.
    PendingMessages {
        recipient: String,
        now: String,
    },
    Context,
    Config,
    Templates,
}

impl Filter {
    pub fn kind(&self) -> EntityKind {
        match self {
            Self::Tasks { .. } => EntityKind::Task,
            Self::Goals { .. } => EntityKind::Goal,
            Self::Agents => EntityKind::Agent,
            Self::Teams => EntityKind::Team,
            Self::Runs { .. } => EntityKind::Run,
            Self::Messages { .. } | Self::PendingMessages { .. } => EntityKind::Message,
            Self::Context => EntityKind::Context,
            Self::Config => EntityKind::Config,
            Self::Templates => EntityKind::Template,
        }
    }
}

/// Default 100, maximum 1,000. Zero is always invalid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageRequest {
    pub limit: usize,
    pub cursor: Option<Cursor>,
    pub sort: Sort,
    pub with_count: bool,
}

impl Default for PageRequest {
    fn default() -> Self {
        Self {
            limit: 100,
            cursor: None,
            sort: Sort::Default,
            with_count: false,
        }
    }
}

impl PageRequest {
    pub fn validate(&self) -> super::Result<()> {
        if !(1..=1000).contains(&self.limit) {
            return Err(super::StoreError::InvalidPage(
                "limit must be 1..=1000".into(),
            ));
        }
        // Also validate cursors constructed by serde rather than `Cursor::new`.
        if let Some(cursor) = &self.cursor {
            Cursor::new(cursor.0.clone())?;
        }
        Ok(())
    }
}

/// Count is the number matching the filter before cursor application.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub next_cursor: Option<Cursor>,
    pub total: Option<u64>,
}
