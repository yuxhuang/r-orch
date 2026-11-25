//! The report of the check. Every text in it is escaped and bounded, because a path,
//! an id and an error message all come from the source (R112: at most 1,000 refusals
//! are listed, with a count of the rest).
use serde::Serialize;

/// The most refusals, and the most notes, a report lists; the rest are counted.
pub const REPORT_LIMIT: usize = 1000;

/// One finding. `rule` is a rule of "Import bounds", or one of this check's own:
/// `file_kind`, `io`, `unreadable`, `id`, `in_flight_live`, `in_flight_remote`,
/// `assume_stopped_unknown`; notes use `not_imported`, `workspace_unclaimed`,
/// `run_event_gaps` and `assumed_stopped`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Finding {
    /// Escaped, relative to the project.
    pub path: String,
    pub rule: &'static str,
    pub limit: Option<u64>,
    /// The value at which the check stopped: the least the source holds. Escaped.
    pub observed: Option<String>,
    /// Escaped and cut; never a value copied whole from a document.
    pub detail: String,
}

#[derive(Debug, Serialize)]
pub struct Capped<T> {
    pub listed: Vec<T>,
    /// How many more there were than `listed` holds.
    pub more: u64,
}

impl<T> Default for Capped<T> {
    fn default() -> Self {
        Self {
            listed: Vec::new(),
            more: 0,
        }
    }
}

impl<T> Capped<T> {
    pub(super) fn push(&mut self, item: T) {
        if self.listed.len() < REPORT_LIMIT {
            self.listed.push(item);
        } else {
            self.more = self.more.saturating_add(1);
        }
    }
    pub fn total(&self) -> u64 {
        self.listed.len() as u64 + self.more
    }
}

/// What the dry run counts. Documents are counted once they have passed every check.
#[derive(Debug, Default, PartialEq, Eq, Serialize)]
pub struct Counts {
    pub entries: u64,
    pub tasks: u64,
    pub agents: u64,
    pub goals: u64,
    pub teams: u64,
    pub messages: u64,
    pub context: u64,
    pub runs: u64,
    pub run_event_files: u64,
    pub run_events: u64,
    /// Runs of events that were not stored: lines over their bound, too deep or not
    /// parsing, with the lines like them that follow counted once (R111).
    pub run_event_gaps: u64,
    pub templates: u64,
    pub attachment_dirs: u64,
    pub attachments: u64,
    pub attachment_bytes: u64,
    pub indexes: u64,
    pub workspaces_listed: u64,
    /// Entries of the source the table of R99 leaves in place for another program.
    pub left_in_place: u64,
    pub running: u64,
    pub claimed: u64,
    pub retry_entries: u64,
    /// Running entries that the import would close in the database (R119).
    pub in_flight_to_close: u64,
}

/// A named run whose process or group still exists: the import goes on only on a typed
/// answer or `--yes` (R119). Not a refusal of the source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Confirmation {
    pub run_id: String,
    pub task_id: String,
    pub pid: u32,
    pub found: &'static str,
}

/// A bounded preview of the order the import would assign, never a load manifest.
#[derive(Debug, Serialize)]
pub struct OrderedEntity {
    pub store: &'static str,
    /// Escaped and bounded, like a finding. Never used as a path.
    pub id: String,
    pub position: u64,
}

#[derive(Debug, Default, Serialize)]
pub struct Report {
    pub counts: Counts,
    /// A preview only. The durable caller must repeat the checked reads under its lock.
    pub order: Capped<OrderedEntity>,
    /// Effective 1.0 run cap; null if config checking did not finish.
    pub run_cap: Option<u64>,
    /// Size of the compact JSON legacy object, not its potentially sensitive values.
    pub legacy_config_bytes: u64,
    /// Input never grants trust: the command must ask the owner before enabling it.
    pub preapproval_requires_confirmation: bool,
    /// A finding here means the check stops: exit 2 with nothing created (R112, R123).
    pub refusals: Capped<Finding>,
    /// Listed and not a stop: no rule for a path, an unclaimed workspace, event gaps.
    pub notes: Capped<Finding>,
    pub confirmations: Vec<Confirmation>,
    /// Run ids the owner said have ended and that the check accepted.
    pub assumed_stopped: Vec<String>,
}

impl Report {
    pub fn refused(&self) -> bool {
        self.refusals.total() > 0
    }
}
