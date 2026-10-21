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
    // Reveal minimally after independently clamping the page and selection.
    state.offset = reveal(state.offset, selected, metrics.selection_rows, height).min(max_offset);
}

/// The offset that shows item `selected` in a viewport `height` rows high,
/// moving `offset` as little as possible. `rows` maps items to display rows;
/// empty means one display row per item.
///
/// Moving up, the rows between the previous item and this one (its section
/// header) come into view with it, as many as fit: row 0 for the first item.
/// The selection itself always shows. Moving down is unchanged: the selection
/// lands on the bottom row, and its header is already above it.
pub(crate) fn reveal(offset: usize, selected: usize, rows: &[usize], height: usize) -> usize {
    let row_of = |index: usize| rows.get(index).copied().unwrap_or(index);
    let row = row_of(selected);
    let top = selected
        .checked_sub(1)
        .map_or(0, |prev| row_of(prev).saturating_add(1))
        .min(row);
    // Subtraction avoids overflowing offset + height for logical usize counts.
    if top < offset {
        top.max(row.saturating_add(1).saturating_sub(height))
    } else if row.saturating_sub(offset) >= height {
        row.saturating_sub(height.saturating_sub(1))
    } else {
        offset
    }
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
        // The first item's header (row 0) comes back with it.
        navigate(&mut state, NavAction::Up, metrics);
        assert_eq!(
            state,
            ScrollState {
                selected: Some(0),
                offset: 0
            }
        );
    }

    /// Two sections of two items: header 0, items 1 2, header 3, items 4 5.
    fn two_sections(height: usize) -> ScrollMetrics<'static> {
        ScrollMetrics {
            items: 4,
            display_rows: 6,
            viewport_rows: height,
            selection_rows: &[1, 2, 4, 5],
            mode: ScrollMode::Selectable,
        }
    }

    #[test]
    fn up_onto_a_section_first_item_above_the_viewport_reveals_its_header() {
        // Selection at the top row, then Up lands on the first item of the
        // second section, which sits above the viewport.
        let mut state = ScrollState {
            selected: Some(3),
            offset: 5,
        };
        navigate(&mut state, NavAction::Up, two_sections(2));
        assert_eq!(
            state,
            ScrollState {
                selected: Some(2),
                offset: 3
            }
        );
    }

    #[test]
    fn up_onto_a_section_first_item_on_the_top_row_reveals_its_header() {
        // From End, the second section's first item is already on screen at
        // the top row; its header just scrolled out above and comes back.
        let mut state = ScrollState::default();
        navigate(&mut state, NavAction::End, two_sections(2));
        assert_eq!(
            state,
            ScrollState {
                selected: Some(3),
                offset: 4
            }
        );
        for (selected, offset) in [(2, 3), (1, 2), (0, 0)] {
            navigate(&mut state, NavAction::Up, two_sections(2));
            assert_eq!(
                state,
                ScrollState {
                    selected: Some(selected),
                    offset
                },
                "Up to item {selected}"
            );
        }
    }

    #[test]
    fn the_selection_stays_visible_when_its_header_does_not_fit() {
        let mut state = ScrollState {
            selected: Some(3),
            offset: 5,
        };
        navigate(&mut state, NavAction::Up, two_sections(1));
        assert_eq!(
            state,
            ScrollState {
                selected: Some(2),
                offset: 4
            }
        );
        // Three stacked rows (0-2) above item 0 at row 3, two rows high:
        // the nearest one and the item show.
        let metrics = ScrollMetrics {
            items: 3,
            display_rows: 6,
            viewport_rows: 2,
            selection_rows: &[3, 4, 5],
            mode: ScrollMode::Selectable,
        };
        let mut state = ScrollState {
            selected: Some(1),
            offset: 4,
        };
        navigate(&mut state, NavAction::Up, metrics);
        assert_eq!(
            state,
            ScrollState {
                selected: Some(0),
                offset: 2
            }
        );
    }

    #[test]
    fn down_through_sections_reveals_exactly_as_before() {
        let mut state = ScrollState::default();
        for (selected, offset) in [(1, 1), (2, 3), (3, 4), (3, 4)] {
            navigate(&mut state, NavAction::Down, two_sections(2));
            assert_eq!(
                state,
                ScrollState {
                    selected: Some(selected),
                    offset
                },
                "Down to item {selected}"
            );
        }
    }

    #[test]
    fn ungrouped_up_reveals_only_the_selected_row() {
        let mut state = ScrollState {
            selected: Some(5),
            offset: 5,
        };
        navigate(&mut state, NavAction::Up, list(10, 3));
        assert_eq!(
            state,
            ScrollState {
                selected: Some(4),
                offset: 4
            }
        );
        navigate(&mut state, NavAction::Up, list(10, 3));
        navigate(&mut state, NavAction::Up, list(10, 3));
        navigate(&mut state, NavAction::Down, list(10, 3));
        assert_eq!(
            state,
            ScrollState {
                selected: Some(3),
                offset: 2
            }
        );
    }

    #[test]
    fn page_up_onto_a_section_first_item_reveals_its_header() {
        // Task-list shape: goal header 0, tasks 1-3, ungrouped header 4,
        // tasks 5-7, show-all row 8. PageUp keeps the selection on the top
        // row, which is the ungrouped section's first task.
        let metrics = ScrollMetrics {
            items: 7,
            display_rows: 9,
            viewport_rows: 2,
            selection_rows: &[1, 2, 3, 5, 6, 7, 8],
            mode: ScrollMode::Selectable,
        };
        let mut state = ScrollState {
            selected: Some(5),
            offset: 7,
        };
        navigate(&mut state, NavAction::PageUp, metrics);
        assert_eq!(
            state,
            ScrollState {
                selected: Some(3),
                offset: 4
            }
        );
    }
}
