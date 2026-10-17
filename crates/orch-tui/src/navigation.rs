//! Shared display-row navigation for selectable lists and read-only text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavAction {
    Up,
    Down,
    PageUp,
    PageDown,
    Home,
    End,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollMode {
    Selectable,
    Text,
}
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScrollState {
    pub selected: Option<usize>,
    pub offset: usize,
}
#[derive(Debug, Clone, Copy)]
pub struct ScrollMetrics<'a> {
    pub items: usize,
    pub display_rows: usize,
    pub viewport_rows: usize,
    pub selection_rows: &'a [usize],
    pub mode: ScrollMode,
}
/// Advance selection and viewport using the rendered body height. An empty
/// selection-row map denotes an ungrouped list with one display row per item.
pub fn navigate(state: &mut ScrollState, action: NavAction, metrics: ScrollMetrics<'_>) {
    let height = metrics.viewport_rows;
    let max_offset = metrics.display_rows.saturating_sub(height);
    state.offset = state.offset.min(max_offset);
    if metrics.mode == ScrollMode::Text {
        if height > 0 {
            state.offset = match action {
                NavAction::Up => state.offset.saturating_sub(1),
                NavAction::Down => state.offset.saturating_add(1).min(max_offset),
                NavAction::PageUp => state.offset.saturating_sub(height),
                NavAction::PageDown => state.offset.saturating_add(height).min(max_offset),
                NavAction::Home => 0,
                NavAction::End => max_offset,
            };
        }
        return;
    }
    if metrics.items == 0 {
        state.selected = None;
        state.offset = 0;
        return;
    }
    let last = metrics.items - 1;
    let selected = state.selected.unwrap_or(0).min(last);
    state.selected = Some(selected);
    if height == 0 {
        return;
    }
    let row_of = |index: usize| metrics.selection_rows.get(index).copied().unwrap_or(index);
    let row = row_of(selected);
    let selected = match action {
        NavAction::Up => selected.saturating_sub(1),
        NavAction::Down => selected.saturating_add(1).min(last),
        NavAction::PageUp | NavAction::PageDown => {
            let target = if action == NavAction::PageDown {
                state.offset = state.offset.saturating_add(height).min(max_offset);
                row.saturating_add(height)
                    .min(metrics.display_rows.saturating_sub(1))
            } else {
                state.offset = state.offset.saturating_sub(height);
                row.saturating_sub(height)
            };
            if metrics.selection_rows.is_empty() {
                target.min(last)
            } else if action == NavAction::PageDown {
                metrics
                    .selection_rows
                    .partition_point(|&row| row < target)
                    .min(last)
            } else {
                metrics
                    .selection_rows
                    .partition_point(|&row| row <= target)
                    .saturating_sub(1)
                    .min(last)
            }
        }
        NavAction::Home => {
            state.offset = 0;
            0
        }
        NavAction::End => {
            state.offset = max_offset;
            last
        }
    };
    state.selected = Some(selected);
    let row = row_of(selected);
    // Reveal minimally after independently clamping the page and selection.
    // Subtraction avoids overflowing offset + height for logical usize counts.
    if row < state.offset {
        state.offset = row;
    } else if row.saturating_sub(state.offset) >= height {
        state.offset = row.saturating_sub(height - 1);
    }
    state.offset = state.offset.min(max_offset);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(items: usize, height: usize) -> ScrollMetrics<'static> {
        ScrollMetrics {
            items,
            display_rows: items,
            viewport_rows: height,
            selection_rows: &[],
            mode: ScrollMode::Selectable,
        }
    }

    #[test]
    fn n1_empty_and_singleton_lists_clamp_for_every_action() {
        for action in [
            NavAction::Up,
            NavAction::Down,
            NavAction::PageUp,
            NavAction::PageDown,
            NavAction::Home,
            NavAction::End,
        ] {
            let mut state = ScrollState {
                selected: Some(99),
                offset: 99,
            };
            navigate(&mut state, action, list(0, 5));
            assert_eq!(
                state,
                ScrollState {
                    selected: None,
                    offset: 0
                }
            );
            navigate(&mut state, action, list(1, 1));
            assert_eq!(
                state,
                ScrollState {
                    selected: Some(0),
                    offset: 0
                }
            );
        }
    }

    #[test]
    fn n2_pages_move_selection_and_viewport_independently() {
        let mut state = ScrollState::default();
        for (action, selected, offset) in [
            (NavAction::PageDown, 10, 10),
            (NavAction::PageDown, 20, 13),
            (NavAction::End, 22, 13),
            (NavAction::Home, 0, 0),
        ] {
            navigate(&mut state, action, list(23, 10));
            assert_eq!(
                state,
                ScrollState {
                    selected: Some(selected),
                    offset
                }
            );
        }
    }

    #[test]
    fn n3_text_pages_clamp_to_visual_rows() {
        let mut state = ScrollState::default();
        let metrics = ScrollMetrics {
            mode: ScrollMode::Text,
            ..list(23, 10)
        };
        for (action, offset) in [
            (NavAction::PageDown, 10),
            (NavAction::PageDown, 13),
            (NavAction::PageUp, 3),
            (NavAction::End, 13),
            (NavAction::Home, 0),
        ] {
            navigate(&mut state, action, metrics);
            assert_eq!(state.offset, offset);
        }
    }

    #[test]
    fn n4_zero_height_clamps_data_without_navigating() {
        for action in [
            NavAction::Up,
            NavAction::Down,
            NavAction::PageUp,
            NavAction::PageDown,
            NavAction::Home,
            NavAction::End,
        ] {
            let mut state = ScrollState {
                selected: Some(99),
                offset: 7,
            };
            navigate(&mut state, action, list(23, 0));
            assert_eq!(
                state,
                ScrollState {
                    selected: Some(22),
                    offset: 7
                }
            );
            let metrics = ScrollMetrics {
                mode: ScrollMode::Text,
                ..list(23, 0)
            };
            navigate(&mut state, action, metrics);
            assert_eq!(state.offset, 7);
        }
    }

    #[test]
    fn n4_extreme_counts_and_resize_do_not_overflow() {
        let mut state = ScrollState {
            selected: Some(usize::MAX),
            offset: usize::MAX,
        };
        navigate(&mut state, NavAction::PageDown, list(usize::MAX, 10));
        assert_eq!(
            state,
            ScrollState {
                selected: Some(usize::MAX - 1),
                offset: usize::MAX - 10
            }
        );
        navigate(&mut state, NavAction::Down, list(usize::MAX, 20));
        assert_eq!(state.offset, usize::MAX - 20);
        navigate(&mut state, NavAction::PageUp, list(usize::MAX, usize::MAX));
        assert_eq!(
            state,
            ScrollState {
                selected: Some(0),
                offset: 0
            }
        );
    }

    #[test]
    fn n5_grouped_pages_include_headers_and_page_up_from_end() {
        let metrics = ScrollMetrics {
            items: 7,
            display_rows: 10,
            viewport_rows: 5,
            selection_rows: &[1, 2, 4, 5, 7, 8, 9],
            mode: ScrollMode::Selectable,
        };
        let mut state = ScrollState::default();
        navigate(&mut state, NavAction::Down, metrics);
        assert_eq!(
            state,
            ScrollState {
                selected: Some(1),
                offset: 0
            }
        );
        state = ScrollState::default();
        for (action, selected, offset) in [
            (NavAction::PageDown, 4, 5),
            (NavAction::End, 6, 5),
            (NavAction::PageUp, 2, 0),
        ] {
            navigate(&mut state, action, metrics);
            assert_eq!(
                state,
                ScrollState {
                    selected: Some(selected),
                    offset
                }
            );
        }
    }

    #[test]
    fn adjacent_moves_minimally_reveal_grouped_selection() {
        let metrics = ScrollMetrics {
            items: 3,
            display_rows: 8,
            viewport_rows: 2,
            selection_rows: &[1, 4, 7],
            mode: ScrollMode::Selectable,
        };
        let mut state = ScrollState::default();
        navigate(&mut state, NavAction::Down, metrics);
        assert_eq!(
            state,
            ScrollState {
                selected: Some(1),
                offset: 3
            }
        );
        navigate(&mut state, NavAction::Up, metrics);
        assert_eq!(
            state,
            ScrollState {
                selected: Some(0),
                offset: 1
            }
        );
    }
}
