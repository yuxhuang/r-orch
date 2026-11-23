//! Attention records (R8, R81–R84, R131). No probes, timers or UI policy.
//!
//! A session with the reserved empty project names the daemon-wide table:
//! only `disk_floor` belongs there. A nonempty project names its local table:
//! only `dispatch_off_with_work` belongs there. Project reads do not implicitly
//! include daemon records. A consumer showing daemon health must read the
//! daemon scope explicitly as well; two sessions are two snapshots.
//!
//! Like the other operational ports, this does not authorize. The dispatcher
//! permits only daemon opens/resolutions/expiry and local owner acknowledgements
//! (R54). Ack identity must come from `Caller.principal`, never request text.
//! No title, detail, remedy, author or resolution text is accepted as input.
//! Times are UTC milliseconds supplied by the daemon, never a clock read here.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::{Result, Revision, StoreError};

#[cfg(feature = "test-support")]
pub mod fake;

pub const MAX_ATTENTION_PAGE: usize = 1_000;
pub const MAX_ATTENTION_DELETE: usize = 1_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttentionKind {
    DiskFloor,
    DispatchOffWithWork,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Blocking,
    Warning,
}

/// A checked id is data, never a path or executable action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DiskSubject {
    DataDirectory,
    ProjectWorkspaces { project: String },
}

/// The only variable item content admitted in 1.1.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AttentionFacts {
    DiskFloor {
        subject: DiskSubject,
        free_bytes: u64,
        floor_bytes: u64,
    },
    DispatchOffWithWork {
        waiting_tasks: u64,
        oldest_waiting_at: i64,
    },
}

pub fn check_id(id: &str) -> Result<()> {
    if id.is_empty()
        || id.len() > 128
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err(StoreError::InvalidAttention("invalid diagnostic id".into()));
    }
    Ok(())
}

pub fn check_time(at: i64) -> Result<()> {
    if at < 0 {
        Err(StoreError::InvalidAttention(
            "time is before the epoch".into(),
        ))
    } else {
        Ok(())
    }
}

impl AttentionFacts {
    pub fn kind(&self) -> AttentionKind {
        match self {
            Self::DiskFloor { .. } => AttentionKind::DiskFloor,
            Self::DispatchOffWithWork { .. } => AttentionKind::DispatchOffWithWork,
        }
    }

    /// Checks scope and facts before even a duplicate-open lookup.
    pub fn validate(&self, project: &str, at: i64) -> Result<()> {
        check_time(at)?;
        match self {
            Self::DiskFloor {
                subject,
                free_bytes,
                floor_bytes,
            } => {
                if !project.is_empty()
                    || *floor_bytes == 0
                    || free_bytes >= floor_bytes
                    || *floor_bytes > i64::MAX as u64
                {
                    return Err(StoreError::InvalidAttention(
                        "invalid disk-floor scope or sizes".into(),
                    ));
                }
                if let DiskSubject::ProjectWorkspaces { project } = subject {
                    check_id(project)?;
                }
            }
            Self::DispatchOffWithWork {
                waiting_tasks,
                oldest_waiting_at,
            } => {
                if project.is_empty()
                    || *waiting_tasks == 0
                    || *waiting_tasks > i64::MAX as u64
                    || *oldest_waiting_at < 0
                    || *oldest_waiting_at > at
                {
                    return Err(StoreError::InvalidAttention(
                        "invalid dispatch scope, count or time".into(),
                    ));
                }
            }
        }
        Ok(())
    }

    /// Stable uniqueness key within kind and scope, matching the schema index.
    pub fn subject_id(&self) -> String {
        match self {
            Self::DiskFloor {
                subject: DiskSubject::DataDirectory,
                ..
            } => "data_directory".into(),
            Self::DiskFloor {
                subject: DiskSubject::ProjectWorkspaces { project },
                ..
            } => format!("workspaces_{project}"),
            Self::DispatchOffWithWork { .. } => "dispatch".into(),
        }
    }

    pub fn severity(&self) -> Severity {
        match self {
            Self::DiskFloor { .. } => Severity::Blocking,
            Self::DispatchOffWithWork { .. } => Severity::Warning,
        }
    }

    pub fn title(&self) -> &'static str {
        match self {
            Self::DiskFloor { .. } => "Free disk space is below the floor",
            Self::DispatchOffWithWork { .. } => "Dispatch is off with work waiting",
        }
    }

    /// Fixed prose plus typed daemon facts, with ids rechecked before display.
    pub fn detail(&self) -> Result<String> {
        self.remedy()?;
        Ok(match self {
            Self::DiskFloor {
                subject,
                free_bytes,
                floor_bytes,
            } => {
                let subject = match subject {
                    DiskSubject::DataDirectory => "data directory".into(),
                    DiskSubject::ProjectWorkspaces { project } => {
                        format!("workspaces of project {project}")
                    }
                };
                format!(
                    "The filesystem for {subject} has {free_bytes} free bytes; the floor is {floor_bytes} bytes."
                )
            }
            Self::DispatchOffWithWork {
                waiting_tasks,
                oldest_waiting_at,
            } => format!(
                "Dispatch is off with {waiting_tasks} tasks waiting; the oldest has waited since UTC millisecond {oldest_waiting_at}."
            ),
        })
    }

    /// Rechecks ids at display time as required by R131, including loaded data.
    pub fn remedy(&self) -> Result<&'static str> {
        if let Self::DiskFloor {
            subject: DiskSubject::ProjectWorkspaces { project },
            ..
        } = self
        {
            check_id(project)?;
        }
        Ok(match self {
            Self::DiskFloor { .. } => "Free disk space on the affected filesystem.",
            Self::DispatchOffWithWork { .. } => "Enable dispatch or remove the waiting work.",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Acknowledgement {
    pub identity: String,
    pub at: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResolutionReason {
    DiskSpaceRecovered,
    DispatchEnabled,
    NoDispatchableWork,
}

impl ResolutionReason {
    pub fn validate(self, kind: AttentionKind) -> Result<()> {
        if matches!(
            (self, kind),
            (Self::DiskSpaceRecovered, AttentionKind::DiskFloor)
                | (
                    Self::DispatchEnabled | Self::NoDispatchableWork,
                    AttentionKind::DispatchOffWithWork
                )
        ) {
            Ok(())
        } else {
            Err(StoreError::InvalidAttention(
                "resolution does not match kind".into(),
            ))
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Resolution {
    pub reason: ResolutionReason,
    pub at: i64,
}

/// The id is the decimal TEXT schema id; allocation is monotonic per scope,
/// never reused after retention. Authorship is always the daemon and resolution
/// is always by the daemon, so neither is caller-supplied mutable data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Attention {
    pub id: u64,
    pub facts: AttentionFacts,
    pub opened_at: i64,
    pub acknowledgement: Option<Acknowledgement>,
    pub resolution: Option<Resolution>,
    pub revision: Revision,
}

impl Attention {
    pub fn check_transition_time(&self, at: i64) -> Result<()> {
        check_time(at)?;
        let last = self
            .opened_at
            .max(self.acknowledgement.as_ref().map_or(0, |a| a.at))
            .max(self.resolution.as_ref().map_or(0, |r| r.at));
        if at < last {
            return Err(StoreError::InvalidAttention(
                "transition time moved backwards".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttentionFilter {
    All,
    Open,
    Resolved,
}

impl AttentionFilter {
    pub fn matches(self, row: &Attention) -> bool {
        match self {
            Self::All => true,
            Self::Open => row.resolution.is_none(),
            Self::Resolved => row.resolution.is_some(),
        }
    }
}

/// A last-id cursor, bound to scope and filter. It retains no snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttentionCursor {
    project: String,
    filter: AttentionFilter,
    after: u64,
}

impl AttentionCursor {
    pub fn after(project: &str, filter: AttentionFilter, id: u64) -> Self {
        Self {
            project: project.into(),
            filter,
            after: id,
        }
    }
    pub fn check(&self, project: &str, filter: AttentionFilter) -> Result<u64> {
        if self.project != project || self.filter != filter {
            return Err(StoreError::InvalidCursor);
        }
        Ok(self.after)
    }
}

pub struct AttentionPage {
    pub items: Vec<Attention>,
    pub next: Option<AttentionCursor>,
}

pub fn check_limit(limit: usize) -> Result<()> {
    if (1..=MAX_ATTENTION_PAGE).contains(&limit) {
        Ok(())
    } else {
        Err(StoreError::InvalidAttention(
            "limit must be in 1..=1000".into(),
        ))
    }
}

#[async_trait]
pub trait AttentionRead: Send + Sync {
    async fn read_attention(&self, id: u64) -> Result<Option<Attention>>;
    /// Ascending id; page repeatedly to enumerate the whole snapshot.
    async fn read_attention_page(
        &self,
        filter: AttentionFilter,
        after: Option<&AttentionCursor>,
        limit: usize,
    ) -> Result<AttentionPage>;
}

pub trait AttentionWrite: Send {
    fn get_attention(&self, id: u64) -> Result<Option<Attention>>;
    fn attention_page(
        &self,
        filter: AttentionFilter,
        after: Option<&AttentionCursor>,
        limit: usize,
    ) -> Result<AttentionPage>;
    /// Returns an existing open row unchanged (including its facts and revision).
    fn open_attention(&mut self, facts: &AttentionFacts, at: i64) -> Result<Attention>;
    /// First acknowledgement wins; a current-revision repeat is a no-op.
    fn ack_attention(&mut self, id: u64, expected: Revision, at: i64) -> Result<Attention>;
    /// A current-revision repeat on a resolved row is a no-op.
    fn resolve_attention(
        &mut self,
        id: u64,
        expected: Revision,
        reason: ResolutionReason,
        at: i64,
    ) -> Result<Attention>;
    /// Deletes at most 1000 resolved rows with resolved_at < cutoff, in id order.
    /// Open and merely acknowledged rows are never removed. Caller computes
    /// cutoff from the configured retention; equality is retained.
    fn expire_attention(&mut self, cutoff: i64, limit: usize) -> Result<usize>;
}
