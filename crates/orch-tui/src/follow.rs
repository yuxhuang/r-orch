//! Tail-follow policy for live feeds. Expanded text remains an immutable snapshot.
use crate::navigation::{NavAction, ScrollState};
use crate::state::{AppState, DetailPanel, Focus, PaneEntity, PaneKey, TaskSection, ViewId};

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

pub(crate) struct TaskActivityAnchor {
    key: PaneKey,
    top: Option<u64>,
    selected: Option<u64>,
}

impl AppState {
    pub fn is_live_feed(&self, focus: Focus) -> bool {
        matches!(
            focus,
            Focus::Activity | Focus::TaskDetail(TaskSection::Activity)
        ) || (focus == Focus::MainList && self.view == ViewId::Logs)
    }

    /// Only task Activity uses an absent selection as its tail sentinel.
    pub fn task_activity_following(&self, key: &PaneKey) -> bool {
        self.pane_scroll(key).selected.is_none()
    }

    pub(crate) fn task_activity_rows(&self, key: &PaneKey) -> Vec<usize> {
        let Some(PaneEntity::Entity(id)) = &key.entity else {
            return Vec::new();
        };
        self.messages
            .iter()
            .enumerate()
            .filter(|(_, m)| m.task_id.as_ref() == Some(id))
            .map(|(i, _)| i)
            .collect()
    }

    pub fn task_activity_scroll(&self, key: &PaneKey, height: usize) -> ScrollState {
        let count = self.task_activity_rows(key).len();
        let mut scroll = self.pane_scroll(key);
        if scroll.selected.is_none() {
            scroll.selected = count.checked_sub(1);
            scroll.offset = count.saturating_sub(height);
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
            Focus::TaskDetail(TaskSection::Activity) => {
                let mut scroll = self.pane_scroll(&key);
                let n = self.task_activity_rows(&key).len();
                if scroll.selected == n.checked_sub(1)
                    || height.is_some_and(|h| scroll.offset >= n.saturating_sub(h))
                {
                    scroll.selected = None;
                    self.set_pane_scroll(key, scroll);
                }
            }
            _ => {}
        }
    }

    pub(crate) fn task_activity_anchor(&self) -> Option<TaskActivityAnchor> {
        let (key, scroll) = self.pane_states.iter().find(|(key, scroll)| {
            key.section == Focus::TaskDetail(TaskSection::Activity) && scroll.selected.is_some()
        })?;
        let rows = self.task_activity_rows(key);
        Some(TaskActivityAnchor {
            key: key.clone(),
            top: rows.get(scroll.offset).map(|i| self.messages[*i].id),
            selected: scroll
                .selected
                .and_then(|s| rows.get(s))
                .map(|i| self.messages[*i].id),
        })
    }

    pub(crate) fn restore_task_activity_anchor(&mut self, anchor: Option<TaskActivityAnchor>) {
        let Some(anchor) = anchor else { return };
        let rows = self.task_activity_rows(&anchor.key);
        let position = |id| {
            rows.iter()
                .position(|i| Some(self.messages[*i].id) == id)
                .unwrap_or(0)
        };
        self.set_pane_scroll(
            anchor.key,
            ScrollState {
                selected: Some(position(anchor.selected)),
                offset: position(anchor.top),
            },
        );
    }
}
