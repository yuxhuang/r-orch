//! Bounded identity-only commit metadata. No wire or storage representation.

use orch_domain::OrchestratorEvent;
use std::sync::Arc;

/// Provenance belongs to the operation task, never to spawned children.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum CommitSource {
    Rpc,
    Tick,
    Collector,
    Peer,
    Expiry,
    #[default]
    Internal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitStore {
    Tasks,
    Goals,
    Agents,
    Messages,
    Context,
    Teams,
    Runs,
    Config,
    State,
    Remote,
}

impl CommitStore {
    pub const ALL: [Self; 10] = [
        Self::Tasks,
        Self::Goals,
        Self::Agents,
        Self::Messages,
        Self::Context,
        Self::Teams,
        Self::Runs,
        Self::Config,
        Self::State,
        Self::Remote,
    ];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitAction {
    Created,
    Updated,
    Deleted,
}

/// Filter projections only; contents, output and secrets never belong here.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Membership {
    pub goal_id: Option<String>,
    pub agent_id: Option<String>,
    pub recipient_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitChange {
    pub store: CommitStore,
    /// None means every possibly affected member of this known store.
    pub id: Option<String>,
    pub action: CommitAction,
    pub old: Option<Membership>,
    pub new: Option<Membership>,
}

impl CommitChange {
    pub fn store_wide(store: CommitStore) -> Self {
        Self {
            store,
            id: None,
            action: CommitAction::Updated,
            old: None,
            new: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitRecord {
    pub commit_seq: u64,
    pub source: CommitSource,
    pub changes: Vec<CommitChange>,
    /// A potentially partial write could not identify its affected stores.
    pub store_set_uncertain: bool,
}

/// One synchronous, ordered path for daemon invalidations and domain events.
/// Publishers must not wait for consumers or re-enter the operation boundary.
#[derive(Debug, Clone)]
pub enum Publication {
    Commit(CommitRecord),
    Bus(Arc<OrchestratorEvent>),
}

pub type Publisher = Arc<dyn Fn(&Publication) + Send + Sync>;

// A large cascade degrades to ten store-wide changes, never an ID history.
const MAX_CHANGES: usize = 256;
const MAX_METADATA_BYTES: usize = 64 * 1024;

#[derive(Default)]
pub(super) struct PendingChanges {
    stores: Vec<CommitStore>,
    changes: Vec<CommitChange>,
    bytes: usize,
    coarse: bool,
}

impl PendingChanges {
    pub(super) fn note(&mut self, store: CommitStore) {
        if !self.stores.contains(&store) {
            self.stores.push(store);
        }
    }

    pub(super) fn record(&mut self, change: CommitChange) {
        self.note(change.store);
        if self.coarse {
            return;
        }
        let membership_bytes = |m: &Option<Membership>| {
            m.as_ref().map_or(0, |m| {
                m.goal_id.as_ref().map_or(0, String::len)
                    + m.agent_id.as_ref().map_or(0, String::len)
                    + m.recipient_id.as_ref().map_or(0, String::len)
            })
        };
        let bytes = change.id.as_ref().map_or(0, String::len)
            + membership_bytes(&change.old)
            + membership_bytes(&change.new);
        if self.changes.len() >= MAX_CHANGES
            || self.bytes.saturating_add(bytes) > MAX_METADATA_BYTES
        {
            self.changes.clear();
            self.bytes = 0;
            self.coarse = true;
            return;
        }
        self.bytes += bytes;
        self.changes.push(change);
    }

    pub(super) fn discard_noop_store(&mut self, store: CommitStore) -> bool {
        if !self.coarse && !self.changes.iter().any(|change| change.store == store) {
            self.stores.retain(|known| *known != store);
        }
        !self.stores.is_empty()
    }

    pub(super) fn finish(self, commit_seq: u64, source: CommitSource) -> CommitRecord {
        let mut changes = if self.coarse {
            Vec::new()
        } else {
            self.changes
        };
        for store in &self.stores {
            if !changes.iter().any(|c| c.store == *store) {
                changes.push(CommitChange::store_wide(*store));
            }
        }
        CommitRecord {
            commit_seq,
            source,
            changes,
            store_set_uncertain: self.stores.is_empty(),
        }
    }
}
