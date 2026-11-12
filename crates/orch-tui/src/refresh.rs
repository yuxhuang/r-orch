//! Which cached views a pushed change makes stale (brief D3/D7).
//!
//! The dashboard caches one read per view. The daemon pushes *which store*
//! changed in *which commit*, never the contents; this module turns that into
//! the set of views to read again, so a frame reads only what a change
//! touched and time alone reads nothing.
//!
//! Each view remembers the commit ordinal its last successful read observed.
//! A change numbered at or below it is already in that read and is not read
//! again; anything newer, any change while the ordinal is unknown, and any
//! change to a view whose last read failed, is. The ordinal is per view on
//! purpose: a goals read at commit 12 says nothing about a tasks read at 10,
//! so a late task change from commit 11 still reads the tasks (brief D3).

use crate::backend::ObservedStore;

/// Minimum interval between timing re-reads driven by Runs/State changes. Event
/// appends can arrive continuously, but each timing read decodes the whole
/// run history. Five seconds bounds that work while keeping live timings
/// current; task and goal changes still use the ordinary debounce.
pub(crate) const TIMINGS_REFRESH_INTERVAL_SECS: i64 = 5;

/// One cached read the dashboard can repeat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Tasks,
    Agents,
    Goals,
    Teams,
    State,
    /// Task and goal Started/Elapsed (`run.timings`).
    Timings,
    PromptMode,
    /// The header's peer line (`remote.status`).
    Peer,
    /// The header's WATCHING / IDLE chip (the daemon's dispatch switch).
    Dispatch,
}

impl View {
    pub const ALL: [View; 9] = [
        View::Tasks,
        View::Agents,
        View::Goals,
        View::Teams,
        View::State,
        View::Timings,
        View::PromptMode,
        View::Peer,
        View::Dispatch,
    ];

    fn bit(self) -> u16 {
        1 << (self as u16)
    }

    /// What a failed read of this view is called in the activity feed.
    pub fn label(self) -> &'static str {
        match self {
            View::Tasks => "Tasks",
            View::Agents => "Agents",
            View::Goals => "Goals",
            View::Teams => "Teams",
            View::State => "Orchestrator state",
            View::Timings => "Task timings",
            View::PromptMode => "Prompt mode",
            View::Peer => "Peer status",
            View::Dispatch => "Dispatch switch",
        }
    }

    /// Whether a commit ordinal can retire a change for this view. The
    /// dispatch switch is daemon memory, not a store: it moves without a
    /// commit, so every state change reads it.
    fn ordinal_applies(self) -> bool {
        self != View::Dispatch
    }
}

/// A set of [`View`]s.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ViewSet(u16);

impl ViewSet {
    pub const EMPTY: ViewSet = ViewSet(0);

    pub fn all() -> ViewSet {
        View::ALL.into_iter().collect()
    }

    pub fn of(views: &[View]) -> ViewSet {
        views.iter().copied().collect()
    }

    pub fn contains(self, view: View) -> bool {
        self.0 & view.bit() != 0
    }

    pub fn insert(&mut self, view: View) {
        self.0 |= view.bit();
    }

    pub fn remove(&mut self, view: View) {
        self.0 &= !view.bit();
    }

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub fn union(self, other: ViewSet) -> ViewSet {
        ViewSet(self.0 | other.0)
    }
}

impl FromIterator<View> for ViewSet {
    fn from_iter<I: IntoIterator<Item = View>>(iter: I) -> Self {
        let mut set = ViewSet::EMPTY;
        for view in iter {
            set.insert(view);
        }
        set
    }
}

/// The views built from `store`.
///
/// Timings derive from tasks, goals, runs and the running entries in state;
/// the agent list carries the cached remote agents; the peer line is the
/// remote links plus the configured listener. The dashboard shows no message
/// or context view, so those stores read nothing.
pub fn views_of(store: ObservedStore) -> ViewSet {
    use View::*;
    match store {
        ObservedStore::Tasks => ViewSet::of(&[Tasks, Timings]),
        ObservedStore::Goals => ViewSet::of(&[Goals, Timings]),
        ObservedStore::Agents => ViewSet::of(&[Agents]),
        ObservedStore::Teams => ViewSet::of(&[Teams]),
        ObservedStore::Runs => ViewSet::of(&[Timings]),
        ObservedStore::State => ViewSet::of(&[State, Timings, Dispatch]),
        ObservedStore::Config => ViewSet::of(&[PromptMode, Peer]),
        ObservedStore::Remote => ViewSet::of(&[Agents, Peer]),
        ObservedStore::Messages | ObservedStore::Context => ViewSet::EMPTY,
    }
}

/// Per-view freshness: the ordinal each view was last read at, the views a
/// change has made dirty, and the views whose last read failed.
#[derive(Debug, Clone, Default)]
pub struct Freshness {
    read_at: [Option<u64>; View::ALL.len()],
    dirty: ViewSet,
    failed: ViewSet,
}

impl Freshness {
    /// A pushed change of `store` at `commit_seq`. Returns whether a view
    /// became dirty.
    pub fn changed(&mut self, store: ObservedStore, commit_seq: u64) -> bool {
        let mut newly = false;
        for view in View::ALL {
            if !views_of(store).contains(view) || self.dirty.contains(view) {
                continue;
            }
            let observed = view.ordinal_applies()
                && !self.failed.contains(view)
                && self.read_at[view as usize].is_some_and(|seen| commit_seq <= seen);
            if !observed {
                self.dirty.insert(view);
                newly = true;
            }
        }
        newly
    }

    /// Every view is stale (a resync): read them all, whatever their ordinal.
    pub fn invalidate_all(&mut self) {
        self.dirty = ViewSet::all();
        self.read_at = Default::default();
    }

    /// `view` was read successfully, observing `seq`.
    pub fn read_ok(&mut self, view: View, seq: Option<u64>) {
        self.read_at[view as usize] = seq;
        self.dirty.remove(view);
        self.failed.remove(view);
    }

    /// A read of `view` failed: it stays stale until the next change of its
    /// store (or a resync) reads it again; no timer retries it.
    pub fn read_failed(&mut self, view: View) {
        self.dirty.remove(view);
        self.failed.insert(view);
    }

    /// Whether `view`'s last read failed.
    pub fn failed(&self, view: View) -> bool {
        self.failed.contains(view)
    }

    /// The views waiting for a read.
    pub fn dirty(&self) -> ViewSet {
        self.dirty
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_observed_change_is_retired_by_the_views_own_ordinal_only() {
        let mut freshness = Freshness::default();
        freshness.read_ok(View::Tasks, Some(10));
        freshness.read_ok(View::Timings, Some(10));
        freshness.read_ok(View::Goals, Some(12));
        assert!(!freshness.changed(ObservedStore::Tasks, 10));
        assert!(freshness.changed(ObservedStore::Tasks, 11));
        assert!(freshness.dirty().contains(View::Tasks));
        assert!(!freshness.dirty().contains(View::Goals));
    }

    #[test]
    fn an_unknown_ordinal_or_a_failed_read_never_retires_a_change() {
        let mut freshness = Freshness::default();
        assert!(freshness.changed(ObservedStore::Teams, 1));
        freshness.read_ok(View::Teams, Some(5));
        freshness.read_failed(View::Teams);
        assert!(freshness.changed(ObservedStore::Teams, 3));
    }

    #[test]
    fn the_dispatch_switch_is_read_on_every_state_change() {
        let mut freshness = Freshness::default();
        for view in View::ALL {
            freshness.read_ok(view, Some(10));
        }
        assert!(freshness.changed(ObservedStore::State, 9));
        assert_eq!(freshness.dirty(), ViewSet::of(&[View::Dispatch]));
    }

    #[test]
    fn a_resync_forgets_every_ordinal() {
        let mut freshness = Freshness::default();
        for view in View::ALL {
            freshness.read_ok(view, Some(10));
        }
        freshness.invalidate_all();
        assert_eq!(freshness.dirty(), ViewSet::all());
        freshness.read_ok(View::Tasks, None);
        assert!(freshness.changed(ObservedStore::Tasks, 1));
    }
}
