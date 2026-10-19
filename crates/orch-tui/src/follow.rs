//! Tail-follow policy for live feeds. Expanded text remains an immutable snapshot.
use std::cmp::Reverse;
use std::collections::HashSet;

use crate::navigation::{NavAction, ScrollState};
use crate::state::{
    AgentSection, AppState, DetailPanel, Focus, GoalSection, PaneEntity, PaneKey, TaskSection,
    ViewId,
};

pub(crate) fn resumes_tail(
    action: NavAction,
    scroll: &ScrollState,
    items: usize,
    height: usize,
    wheel: bool,
) -> bool {
    action == NavAction::End
        || (matches!(action, NavAction::Down | NavAction::PageDown)
            && (scroll.selected == items.checked_sub(1)
                || ((wheel || action == NavAction::PageDown)
                    && height > 0
                    && scroll.offset >= items.saturating_sub(height))))
}

/// Where a paused detail Activity pane stood, by message id, so arrivals and
/// evictions move the window with its content instead of by index.
pub(crate) struct DetailActivityAnchor {
    key: PaneKey,
    top: Option<u64>,
    selected: Option<u64>,
    /// Display positions before the change, for the eviction clamp.
    offset: usize,
    position: usize,
}

/// Task, Goal and Agent detail Activity panes share one follow/scroll shape.
pub(crate) fn is_detail_activity(section: Focus) -> bool {
    matches!(
        section,
        Focus::TaskDetail(TaskSection::Activity)
            | Focus::GoalDetail(GoalSection::Activity)
            | Focus::AgentDetail(AgentSection::Activity)
    )
}

/// Goal and Agent Activity list newest first, so their live edge is the top.
/// Task Activity keeps the feed's chronology, with its live edge at the bottom.
pub(crate) fn detail_activity_newest_first(section: Focus) -> bool {
    matches!(
        section,
        Focus::GoalDetail(GoalSection::Activity) | Focus::AgentDetail(AgentSection::Activity)
    )
}

/// `resumes_tail` with the live edge at the top: Home, or reaching row 0.
pub(crate) fn resumes_head(
    action: NavAction,
    scroll: &ScrollState,
    items: usize,
    wheel: bool,
) -> bool {
    action == NavAction::Home
        || (matches!(action, NavAction::Up | NavAction::PageUp)
            && (items == 0
                || scroll.selected == Some(0)
                || ((wheel || action == NavAction::PageUp) && scroll.offset == 0)))
}

/// The resume policy for a detail Activity pane, oriented to its live edge.
pub(crate) fn resumes_detail_activity(
    section: Focus,
    action: NavAction,
    scroll: &ScrollState,
    items: usize,
    height: usize,
    wheel: bool,
) -> bool {
    if detail_activity_newest_first(section) {
        resumes_head(action, scroll, items, wheel)
    } else {
        resumes_tail(action, scroll, items, height, wheel)
    }
}

impl AppState {
    pub fn is_live_feed(&self, focus: Focus) -> bool {
        matches!(focus, Focus::Activity)
            || is_detail_activity(focus)
            || (focus == Focus::MainList && self.view == ViewId::Logs)
    }

    /// Detail Activity panes use an absent selection as their follow sentinel.
    pub fn detail_activity_following(&self, key: &PaneKey) -> bool {
        self.pane_scroll(key).selected.is_none()
    }

    /// Message indices of a detail Activity pane, in display order. Membership
    /// is structural only: task = the message's task id; agent = its agent id;
    /// goal = its task id names any task of the goal (all tasks, whatever the
    /// main list filters). Goal and Agent are newest first (timestamp, then id).
    pub fn detail_activity_rows(&self, key: &PaneKey) -> Vec<usize> {
        let Some(PaneEntity::Entity(id)) = &key.entity else {
            return Vec::new();
        };
        let mut rows: Vec<usize> = match key.section {
            Focus::TaskDetail(TaskSection::Activity) => self
                .messages
                .iter()
                .enumerate()
                .filter(|(_, m)| m.task_id.as_ref() == Some(id))
                .map(|(i, _)| i)
                .collect(),
            Focus::AgentDetail(AgentSection::Activity) => self
                .messages
                .iter()
                .enumerate()
                .filter(|(_, m)| m.agent_id.as_ref() == Some(id))
                .map(|(i, _)| i)
                .collect(),
            Focus::GoalDetail(GoalSection::Activity) => {
                let tasks: HashSet<&str> = self
                    .tasks
                    .iter()
                    .filter(|t| t.goal_id.as_deref() == Some(id.as_str()))
                    .map(|t| t.id.as_str())
                    .collect();
                self.messages
                    .iter()
                    .enumerate()
                    .filter(|(_, m)| m.task_id.as_deref().is_some_and(|t| tasks.contains(t)))
                    .map(|(i, _)| i)
                    .collect()
            }
            _ => return Vec::new(),
        };
        if detail_activity_newest_first(key.section) {
            rows.sort_by_key(|i| {
                let m = &self.messages[*i];
                (Reverse(m.ts_ms), Reverse(m.id))
            });
        }
        rows
    }

    /// The pane's scroll with the follow sentinel resolved to its live edge.
    pub fn detail_activity_scroll(&self, key: &PaneKey, height: usize) -> ScrollState {
        let count = self.detail_activity_rows(key).len();
        let mut scroll = self.pane_scroll(key);
        if scroll.selected.is_none() {
            if detail_activity_newest_first(key.section) {
                scroll.selected = (count > 0).then_some(0);
                scroll.offset = 0;
            } else {
                scroll.selected = count.checked_sub(1);
                scroll.offset = count.saturating_sub(height);
            }
        } else {
            scroll.selected = scroll.selected.map(|i| i.min(count.saturating_sub(1)));
            scroll.offset = scroll.offset.min(count.saturating_sub(height));
        }
        scroll
    }

    pub(crate) fn resume_feed_at_bottom(&mut self, focus: Focus) {
        let key = self.pane_key(focus);
        let height = self
            .mouse
            .hits
            .metrics
            .panes
            .iter()
            .find(|m| m.key == key)
            .map(|m| usize::from(m.body.height));
        match focus {
            Focus::Activity => {
                let rows = self.activity_messages();
                if rows
                    .last()
                    .is_none_or(|i| Some(self.messages[*i].id) == self.activity_selected)
                    || height.is_some_and(|h| self.activity_scroll >= rows.len().saturating_sub(h))
                {
                    self.activity_follow_tail = true;
                }
            }
            Focus::MainList if self.view == ViewId::Logs && self.detail != DetailPanel::Log => {
                let n = self.filtered_messages().len();
                if self.log_selected_index >= 0
                    && (self.log_selected_index as usize >= n.saturating_sub(1)
                        || height.is_some_and(|h| self.log_scroll_offset >= n.saturating_sub(h)))
                {
                    self.log_selected_index = -1;
                }
            }
            focus if is_detail_activity(focus) => {
                let mut scroll = self.pane_scroll(&key);
                let n = self.detail_activity_rows(&key).len();
                let at_edge = if detail_activity_newest_first(focus) {
                    scroll.selected == Some(0) || scroll.offset == 0
                } else {
                    scroll.selected == n.checked_sub(1)
                        || height.is_some_and(|h| scroll.offset >= n.saturating_sub(h))
                };
                if at_edge {
                    scroll.selected = None;
                    self.set_pane_scroll(key, scroll);
                }
            }
            _ => {}
        }
    }

    /// Every paused detail Activity pane (at most one entity per view).
    pub(crate) fn detail_activity_anchors(&self) -> Vec<DetailActivityAnchor> {
        self.pane_states
            .iter()
            .filter(|(key, scroll)| is_detail_activity(key.section) && scroll.selected.is_some())
            .map(|(key, scroll)| {
                let rows = self.detail_activity_rows(key);
                let position = scroll.selected.unwrap_or(0);
                DetailActivityAnchor {
                    key: key.clone(),
                    top: rows.get(scroll.offset).map(|i| self.messages[*i].id),
                    selected: rows.get(position).map(|i| self.messages[*i].id),
                    offset: scroll.offset,
                    position,
                }
            })
            .collect()
    }

    /// Re-find each anchor by message id. An evicted anchor clamps toward the
    /// oldest edge it was nearest: the top for chronological Task Activity
    /// (oldest rows are evicted from the top), its old position bounded by the
    /// new length for newest-first Goal/Agent Activity (evicted from the bottom).
    pub(crate) fn restore_detail_activity_anchors(&mut self, anchors: Vec<DetailActivityAnchor>) {
        for anchor in anchors {
            let rows = self.detail_activity_rows(&anchor.key);
            let newest_first = detail_activity_newest_first(anchor.key.section);
            let position = |id: Option<u64>, old: usize| {
                rows.iter()
                    .position(|i| Some(self.messages[*i].id) == id)
                    .unwrap_or(if newest_first {
                        old.min(rows.len().saturating_sub(1))
                    } else {
                        0
                    })
            };
            let scroll = ScrollState {
                selected: Some(position(anchor.selected, anchor.position)),
                offset: position(anchor.top, anchor.offset),
            };
            self.set_pane_scroll(anchor.key, scroll);
        }
    }
}
